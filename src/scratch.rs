//! Scratch ownership, and reaping what a member left behind.
//!
//! The rules themselves — the ownership lock, descendant reaping, and the
//! successor contract — are stated once, in `docs/contract.md`'s liveness
//! sentence, and are not restated here: a copy of a contract is a copy that
//! drifts, and `tests/contract.rs` gates the document against this crate's own
//! constants rather than against prose. What this module adds is *why* each is
//! shaped the way it is. All three are ported from ai-orchestrator intact, and
//! each exists because a simpler rule destroyed live work:
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
//!   sweeper reclaims a directory only when a *non-blocking exclusive* kernel
//!   lock on [`OWNER_LOCK_FILE`] is granted — the kernel's own answer to "can
//!   anything still be using this?", spelled `flock` on POSIX and `LockFileEx`
//!   on Windows — **and** the recorded identity no longer names a live process.
//!   Anything the proof does not clear is retained, not removed.
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
//! Windows has none of the four POSIX facilities the rules were written on, so
//! each is answered by the equivalent that preserves the same safety property:
//!
//! | rule | POSIX | Windows |
//! | --- | --- | --- |
//! | ownership lock | `flock(LOCK_EX \| LOCK_NB)` | `LockFileEx(EXCLUSIVE \| FAIL_IMMEDIATELY)` |
//! | start token | `/proc/<pid>/stat`, `libproc` | `GetProcessTimes` creation time |
//! | tree membership | an environment stamp fixed at `exec` | a job object every child joins |
//! | teardown | `SIGTERM`, grace, `SIGKILL` | `TerminateJobObject` |
//!
//! The first two are the same guarantee by another name. The other two are the
//! two places the guarantee is *not* identical, and a consumer supervising on
//! Windows should know which:
//!
//! * **There is no signal to decline.** `SIGTERM`-then-`SIGKILL` is an ask
//!   followed by a compulsion, and Windows has only the compulsion. What a
//!   member loses is the chance to shut itself down cleanly first; what a caller
//!   of [`reap`] is promised — nothing stamped for this scratch is still running
//!   when it returns — is unchanged.
//! * **A group is created by its launcher**, not fixed at `exec`. So a scratch
//!   nobody launched into has no group to find, and [`stamped_for`] reports
//!   nothing rather than guessing — the direction that retains a directory
//!   rather than killing something still using it.
//!
//! On a platform that is neither, there is no kernel lock to degrade *to*: a
//! claim is granted on the directory's own existence, and taking it proves
//! nothing, so [`reclaimable`] retains every directory and reaping falls back to
//! the child the supervisor holds. That is the safe direction of each answer —
//! nothing is reported live that cannot be *proven* live, and nothing is
//! reclaimed or killed on an unproven one.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

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
// llmlint: ignore-block[invalid_states_unrepresentable] both fields are
// deliberately unvalidated, because this type is a *record* of two numbers read
// back off disk rather than a claim about them. The pair is the whole of
// `owner.lock`'s format — `docs/contract.md` names it "pid-with-start-token" —
// and narrowing either field would change a serialized contract to describe a
// state nothing acts on. Nothing here trusts a value: the only reader is
// [`is_live`], which asks the kernel for the token `pid` holds *now* and
// compares, so an out-of-range pid and an absent token both answer "not live"
// by the same path a stale one does. That is also the direction that retains a
// directory rather than reclaiming one still in use, and
// `an_identity_answers_for_the_process_it_was_taken_from` holds it against a
// pid that cannot exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessIdentity {
    /// The process id.
    pub pid: i32,
    /// The kernel's start token for the process that held it when this identity
    /// was taken.
    pub start_token: u64,
}
// llmlint: ignore-end[invalid_states_unrepresentable]

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

/// One member's process tree, grouped so that it can be torn down whole.
///
/// The tree is what has to end, not the child this process happens to hold: a
/// member is `onejudge`, which spawns `oneharness`, which spawns the paid
/// provider — and killing the first leaves the other two running, still holding
/// the pipes this supervisor is reading and still billing whoever owns the
/// subscription.
///
/// The two platforms prove membership by opposite means, and a [`Group`] is the
/// one seam that hides the difference:
///
/// * **POSIX** — the group *is* the [`SCRATCH_ENV`] stamp. The kernel fixes an
///   environment at `exec`, so every descendant carries it whether or not its
///   parent is still alive, and joining is nothing more than carrying the stamp
///   into the spawn.
/// * **Windows** — the group is a **job object**. There is no environment a
///   sweeper can read out of another process here, and no process group to
///   signal; a job is the kernel's own answer to "everything this process
///   started", it holds a descendant whose parent has already exited, and
///   `TerminateJobObject` ends the whole tree at once.
///
/// Dropping a group is *not* nothing on Windows: the job is created
/// `KILL_ON_JOB_CLOSE`, so a supervisor that panics or is killed outright takes
/// its member's tree with it rather than leaking a paid harness.
#[derive(Debug)]
pub struct Group {
    /// The scratch this group belongs to, which is also its name.
    scratch: PathBuf,
    /// What the platform holds the group open with.
    inner: platform::Group,
}

impl Group {
    /// Open the group `scratch` names, creating whatever proves membership.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] when the platform's grouping facility cannot be
    /// created. A member launched outside its group is one a cancel cannot
    /// reach, so this refuses rather than proceeding ungrouped.
    pub fn open(scratch: &Path) -> Result<Self, Error> {
        Ok(Self {
            scratch: scratch.to_path_buf(),
            inner: platform::Group::open(scratch)?,
        })
    }

    /// Spawn `command` inside this group, so everything it starts joins too.
    ///
    /// # Errors
    ///
    /// Whatever [`Command::spawn`] reports, and — on a platform that has to put
    /// the child in the group *before* it runs — the failure to do so. A child
    /// that started but could not be grouped is killed rather than returned.
    pub fn spawn(&self, command: &mut Command) -> std::io::Result<Child> {
        self.prepare(command)?;
        let mut child = command.spawn()?;
        match self.adopt(&child) {
            Ok(_) => Ok(child),
            // A member running unsupervised is worse than one that never
            // started: a harness no cancel can reach keeps billing.
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(err)
            }
        }
    }

    /// The half of joining a group that has to happen *before* the fork.
    ///
    /// Split out of [`Group::spawn`] because a caller that does not own the
    /// `spawn` call needs the same two moments in the same order —
    /// [`crate::judge`] hands them to onejudge's `SpawnHook`, whose two methods
    /// are exactly this split, for exactly this reason.
    ///
    /// The [`SCRATCH_ENV`] stamp is applied here rather than by each caller
    /// because on POSIX the stamp **is** the group: a command that reaches the
    /// kernel without it is outside the group no matter what this returns.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports when a command cannot be configured for its
    /// group; the spawn is then refused rather than made ungrouped.
    pub(crate) fn prepare(&self, command: &mut Command) -> std::io::Result<()> {
        command.env(SCRATCH_ENV, self.scratch.display().to_string());
        self.inner.prepare(command)
    }

    /// The half that has to happen *after* the process exists, answering the
    /// name of the group it was placed in.
    ///
    /// The name is the scratch directory, which is what a group is called on
    /// both platforms — [`stamped_for`] and [`reap`] take exactly this string,
    /// so a record of it is a record something else can act on.
    ///
    /// For a caller that spawned the child itself, and so owns the moment it
    /// starts running. A caller that only *observes* somebody else's spawn wants
    /// [`Group::join`]; the two differ by exactly the resume, and getting that
    /// wrong is a Windows-only race — see there.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports when a live process cannot be put in the
    /// group. The caller kills the child rather than running it ungrouped.
    pub(crate) fn adopt(&self, child: &Child) -> std::io::Result<String> {
        let name = self.join(child)?;
        self.inner.start(child)?;
        Ok(name)
    }

    /// [`Group::adopt`] for a caller that did not spawn the child and must not
    /// start it: the process joins this group and is left exactly as it was
    /// found.
    ///
    /// This is the half a spawn *hook* wants, and on Windows the distinction is
    /// load-bearing rather than tidy. `oneharness_core`'s `ProcessSupervisor`
    /// calls its `spawned` hook while the child is still `CREATE_SUSPENDED` and
    /// resumes it itself once the hook returns — precisely so a job assignment
    /// cannot miss a descendant. A hook that resumed here would let the child run
    /// and possibly exit before oneharness enumerated its primary thread, and
    /// upstream reads a thread it cannot find as a spawn failure and tears the
    /// tree down. So the group takes the process and leaves the starting to
    /// whoever owns it.
    ///
    /// # Errors
    ///
    /// Whatever the platform reports when a live process cannot be put in the
    /// group. The caller refuses the member rather than running it ungrouped.
    pub(crate) fn join(&self, child: &Child) -> std::io::Result<String> {
        self.inner.join(child)?;
        Ok(self.scratch.display().to_string())
    }

    /// Terminate every live process in this group, and say how many.
    pub fn terminate(&self) -> usize {
        self.inner.terminate(&self.scratch)
    }
}

/// The identity a scratch directory's lock file records.
///
/// A lock file is external input — a scratch directory is on disk, and anything
/// with write access to it can put a number here — so the pid is range-checked
/// rather than merely parsed. Only a *positive* pid names a process on either
/// platform, and the two ways a non-positive one would be read are both bad: on
/// POSIX a negative pid is how `kill` addresses a whole process group, and on
/// Windows it would be widened into an unrelated but perfectly real pid. Neither
/// path is reachable from here today — this identity only ever reaches
/// [`is_live`] — but the number is rejected at the boundary rather than trusted
/// to stay unreachable.
///
/// The shape is checked as strictly as the number. [`Owned::claim`] writes
/// exactly two fields, so a file carrying anything after them is one this crate
/// did not write — and reading the first two numbers out of it anyway would take
/// a stranger's word for who owns the directory.
///
/// A record that fails any of this proves nothing about a live process, which
/// leaves the lock as the only proof; see [`reclaimable`] for what that decides.
//
// llmlint: ignore-block[changed_behavior_has_e2e] the range check has no journey
// because no journey can reach it. Its only caller is [`reclaimable`], which
// `oneagentgraph sweep` does drive end to end — but only over lock files this
// crate itself wrote, and `Owned::claim` cannot write one of these. Reaching it
// through the verb would mean forging a lock file in a swept directory and
// asserting that nothing happened, which is a unit test with a subprocess
// wrapped around it rather than a journey. What the boundary is worth holding is
// held below — both that a non-positive record is refused and that the refusal
// is a range check rather than a blanket one — and the sweep journeys in
// tests/e2e/liveness.rs hold what an operator can actually reach.
fn recorded_identity(lock_path: &Path) -> Option<ProcessIdentity> {
    let recorded = std::fs::read_to_string(lock_path).ok()?;
    let mut parts = recorded.split_whitespace();
    let pid: i32 = parts.next()?.parse().ok()?;
    let start_token = parts.next()?.parse().ok()?;
    if pid <= 0 || parts.next().is_some() {
        return None;
    }
    Some(ProcessIdentity { pid, start_token })
}
// llmlint: ignore-end[changed_behavior_has_e2e]

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

    /// How much CPU `pid` has been charged so far, **in microseconds**.
    ///
    /// One unit on every platform, because the rule this feeds is a rate — see
    /// [`Work::worked`] — and a rate has to be in something. Which unit
    /// matters less than that all three agree on it: microseconds is finer than
    /// the threshold can resolve over any window this is sampled at, so the two
    /// platforms counting below it round away nothing the rule can see, and the
    /// one counting above it converts exactly.
    #[cfg(target_os = "linux")]
    pub fn consumed(pid: i32) -> Option<u64> {
        // Fields 14 and 15 of `/proc/<pid>/stat`: the user and system time this
        // process has been charged, in the kernel's own ticks. Parsed from after
        // the last `)` for the reason [`start_token`] gives — field 2 is an
        // executable name that may itself contain spaces and parentheses.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let tail = &stat[stat.rfind(')')? + 1..];
        let mut fields = tail.split_whitespace();
        let user: u64 = fields.nth(11)?.parse().ok()?;
        let system: u64 = fields.next()?.parse().ok()?;
        Some(
            user.saturating_add(system)
                .saturating_mul(micros_per_tick()?),
        )
    }

    /// How long one of those ticks is, asked of the platform rather than assumed.
    ///
    /// It is a hundred per second on every ordinary Linux build, but it is an
    /// ABI constant of the *platform* rather than of the kernel — 1024 on alpha,
    /// and settable elsewhere — so it is read. A platform that will not answer,
    /// or answers something a tick cannot be, yields no reading rather than one
    /// in a unit nobody checked: an unreadable process counts as nothing, exactly
    /// as it does when `/proc` cannot be parsed, and that is the direction that
    /// still condemns a wedged member.
    // llmlint: ignore-block[changed_behavior_has_e2e] no journey can reach the
    // refusal below: `sysconf(_SC_CLK_TCK)` is a constant of any host that can
    // run this suite. It degrades to the reading-less path a `/proc` parse
    // failure already takes, which the journeys do cover.
    #[cfg(target_os = "linux")]
    fn micros_per_tick() -> Option<u64> {
        // SAFETY: `sysconf` reads a constant of the running platform into its
        // return value; nothing is borrowed and nothing is written.
        let per_second = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).ok()?;
        (1..=1_000_000)
            .contains(&per_second)
            .then(|| 1_000_000 / per_second)
    }
    // llmlint: ignore-end[changed_behavior_has_e2e]

    /// The same reading from `libproc`, for the platform with no `/proc` to read
    /// it out of.
    ///
    /// `proc_taskinfo` counts in nanoseconds, which is precise enough to see the
    /// bookkeeping wake-ups of an otherwise parked harness — a resolution the
    /// Linux counter does not have. That difference is why this reading is no
    /// longer compared for *equality* anywhere: a rule that asked whether the
    /// number moved at all read the same idle member as busy here and idle
    /// there, and coarsening this counter to hide that was calibrating a rule
    /// whose shape was wrong. [`Work::worked`] asks how *fast* it moved
    /// instead, and this reports the same microseconds every platform does.
    #[cfg(target_vendor = "apple")]
    pub fn consumed(pid: i32) -> Option<u64> {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = c_int::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
        // SAFETY: `proc_pidinfo` fills at most `size` bytes of the buffer, which
        // is a live `proc_taskinfo` this frame owns, and reports how many it
        // wrote. A short write is rejected below rather than read.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKINFO,
                0,
                std::ptr::from_mut(&mut info).cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        const NANOS_PER_MICRO: u64 = 1_000;
        Some(info.pti_total_user.saturating_add(info.pti_total_system) / NANOS_PER_MICRO)
    }

    /// No CPU accounting to be had here, so a member's silence is judged by its
    /// stream alone — the behaviour every platform had before this reading
    /// existed.
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    pub fn consumed(_pid: i32) -> Option<u64> {
        None
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
        if !super::is_live(identity) {
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
    const TERM: i32 = libc::SIGTERM;
    /// The signal nothing survives.
    const KILL: i32 = libc::SIGKILL;

    /// How long a proven descendant is given to shut itself down before it is
    /// killed. Short, because everything here has already been asked to stop.
    const GRACE: std::time::Duration = std::time::Duration::from_millis(200);

    /// Ask every target to stop, then kill whatever declined.
    ///
    /// Each signal revalidates the identity, so a member that shut itself down
    /// on the first is never sent a second, and a number its exit released is
    /// never signalled at all.
    pub fn terminate(_scratch: &std::path::Path, targets: &[ProcessIdentity]) -> usize {
        let mut signalled = 0;
        for identity in targets {
            if signal(*identity, TERM) {
                signalled += 1;
            }
        }
        std::thread::sleep(GRACE);
        for identity in targets {
            signal(*identity, KILL);
        }
        signalled
    }

    /// A member's process tree, which here is just its [`super::SCRATCH_ENV`]
    /// stamp: the kernel fixes an environment at `exec`, so a descendant is in
    /// the group by construction and there is nothing to create or hold open.
    #[derive(Debug)]
    pub struct Group;

    impl Group {
        /// Nothing to open: the stamp is applied by the caller's `Command`.
        pub fn open(_scratch: &std::path::Path) -> Result<Self, crate::error::Error> {
            Ok(Self)
        }

        /// Nothing to configure: the caller's stamp is the whole membership.
        pub fn prepare(&self, _command: &mut std::process::Command) -> std::io::Result<()> {
            Ok(())
        }

        /// Nothing to do to a live process either — it was in the group the
        /// moment it `exec`ed, and it cannot be moved out of one.
        pub fn join(&self, _child: &std::process::Child) -> std::io::Result<()> {
            Ok(())
        }

        /// Nothing to start: a POSIX child is running from its `exec`, so the
        /// suspend/resume window Windows needs does not exist here.
        pub fn start(&self, _child: &std::process::Child) -> std::io::Result<()> {
            Ok(())
        }

        /// Reap everything still stamped for this scratch.
        pub fn terminate(&self, scratch: &std::path::Path) -> usize {
            super::reap(scratch)
        }
    }

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

/// The same four proofs on Windows, which has none of the POSIX facilities they
/// were written on.
///
/// Two of the four map across exactly, and are the same guarantee by another
/// name: `LockFileEx` is a kernel byte-range lock the way `flock` is a kernel
/// file lock, and a process's `GetProcessTimes` creation time is fixed when it
/// starts the way a Linux start jiffy or a Darwin start microsecond is. Both are
/// refused to a process that is not the owner and survive a recycled pid.
///
/// The other two cannot be expressed the same way, and this is exactly what
/// differs:
///
/// * **There is no environment stamp a sweeper can read.** A process's
///   environment lives in its own address space here, reachable only by reading
///   another process's memory through an undocumented structure. So membership
///   is proven by a **job object** instead — the kernel's own record of
///   "everything this process started". A job holds a descendant whose parent
///   has already exited, exactly as the `exec`-fixed stamp does, and it is
///   stronger in one respect: a descendant cannot leave a job, whereas a
///   descendant that re-`exec`s with a scrubbed environment sheds a stamp. It is
///   weaker in another: a job is created *by the launcher*, so a scratch nobody
///   launched into — one left by a build that predates this, or by a crash
///   between creating the directory and creating the job — has no job to find,
///   and `stamped_for` reports nothing rather than guessing. That direction
///   retains rather than kills, which is the safe one.
/// * **There is no signal to decline.** `SIGTERM`-then-`SIGKILL` is an ask
///   followed by a compulsion, and Windows has only the compulsion:
///   `TerminateJobObject` ends the whole tree at once and cannot be refused. The
///   safety property — nothing stamped for this scratch is still running when
///   the reap returns — holds identically; what a member loses is the chance to
///   shut itself down cleanly first. The e2e journey named for a descendant
///   that refuses to stop is POSIX-only for that reason: its subject is the
///   escalation, and here there is nothing to escalate past.
///
/// A job is created `KILL_ON_JOB_CLOSE`, so the tree also dies with the
/// supervisor that launched it — a leaked paid harness is what the whole rule
/// exists to prevent, and a supervisor killed outright cannot run a reap.
#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::fs::File;
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use std::os::windows::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command};

    use sha2::{Digest as _, Sha256};
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_MORE_DATA, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, SYNCHRONIZE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicProcessIdList,
        JobObjectExtendedLimitInformation, OpenJobObjectW, QueryInformationJobObject,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::SystemServices::{JOB_OBJECT_QUERY, JOB_OBJECT_TERMINATE};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, OpenThread, ResumeThread, WaitForSingleObject,
        CREATE_SUSPENDED, PROCESS_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
    };

    use super::ProcessIdentity;
    use crate::error::Error;

    /// The file, inside a scratch directory, naming the job object that scratch's
    /// process tree belongs to.
    ///
    /// A job object is reachable by name, and this is where the name is written —
    /// the thing that lets a *second* process (an operator's `cancel --kill`)
    /// find the tree a first one launched. It sits beside `owner.lock` for the
    /// same reason: the scratch directory is the run's own record of itself.
    const OWNER_JOB_FILE: &str = "owner.job";

    /// How far below a scratch a walk looks for a group's record.
    ///
    /// A run's own layout puts a member's scratch at `<run>/members/<name>`, two
    /// levels down, and the harnesses working under it put arbitrary trees below
    /// that. The bound is what keeps `cancel` on a run whose members have been
    /// writing for an hour from walking their output; nothing this crate creates
    /// records a group deeper than that.
    const RECORD_DEPTH: usize = 4;

    /// The byte a claim is taken on: one byte, at an offset no `owner.lock` will
    /// ever reach.
    ///
    /// A Windows byte-range lock is *mandatory* where `flock` is advisory, so
    /// locking the bytes the record is written in would make the record itself
    /// unreadable to everyone but the owner — and the record is the second half
    /// of the proof, which a sweeper has to read while the first half is still
    /// held. Locking past the end of the file instead, which the API explicitly
    /// permits, keeps the lock a pure semaphore: mutual exclusion between
    /// claimants, and no bearing on anybody reading the file.
    const CLAIM_OFFSET_HIGH: u32 = 0x8000_0000;

    /// Take a non-blocking exclusive byte-range lock, reporting whether it was
    /// granted.
    pub fn try_lock_exclusive(file: &File) -> bool {
        let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED =
            unsafe { std::mem::zeroed() };
        overlapped.Anonymous.Anonymous.OffsetHigh = CLAIM_OFFSET_HIGH;
        // SAFETY: the handle is one this process owns for the lifetime of the
        // borrow, and `overlapped` is a live structure this frame owns carrying
        // the offset the range starts at. Failure is reported through the
        // return value.
        unsafe {
            LockFileEx(
                file.as_raw_handle() as HANDLE,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &raw mut overlapped,
            ) != 0
        }
    }

    /// The kernel's start token for `pid`: the wall-clock time that process
    /// started, in 100-nanosecond ticks, which a recycled number never repeats.
    ///
    /// `None` for a pid this process cannot open — one that has exited, or one
    /// belonging to another user, neither of which this run could have launched.
    /// A process that has *finished* but whose handle somebody still holds also
    /// answers `None`: its creation time is still readable, so the wait below is
    /// what separates "still that process" from "was that process".
    pub fn start_token(pid: i32) -> Option<u64> {
        // Checked rather than `as`: a lossy widening turns a number that names
        // no process into one that may name a real and unrelated one.
        let pid = u32::try_from(pid).ok()?;
        // SAFETY: `OpenProcess` takes a pid and reports failure with a null
        // handle; nothing is borrowed.
        let handle =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: `handle` is a live process handle this frame owns until the
        // `CloseHandle` below, and every out-parameter is a live local.
        let (running, times) = unsafe {
            let running = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
            let mut created = std::mem::zeroed();
            let mut exited = std::mem::zeroed();
            let mut kernel = std::mem::zeroed();
            let mut user = std::mem::zeroed();
            let read = GetProcessTimes(
                handle,
                &raw mut created,
                &raw mut exited,
                &raw mut kernel,
                &raw mut user,
            );
            CloseHandle(handle);
            (running, (read != 0).then_some(created))
        };
        if !running {
            return None;
        }
        let created = times?;
        Some((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }

    /// How much CPU `pid` has been charged so far, **in microseconds**.
    ///
    /// The same call [`start_token`] reads a creation time out of, asked for the
    /// other two times it returns. `FILETIME` counts hundred-nanosecond ticks;
    /// the unit every platform reports in is microseconds, for the reason the
    /// Linux reading gives — [`Work::worked`] judges a rate, and a rate is
    /// in something.
    pub fn consumed(pid: i32) -> Option<u64> {
        // Checked rather than `as`, for the reason [`start_token`] gives: a lossy
        // widening turns a number that names no process into one that may name a
        // real and unrelated one.
        let pid = u32::try_from(pid).ok()?;
        // SAFETY: `OpenProcess` takes a pid and reports failure with a null
        // handle; nothing is borrowed.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: `handle` is a live process handle this frame owns until the
        // `CloseHandle` below, and every out-parameter is a live local.
        let times = unsafe {
            let mut created = std::mem::zeroed();
            let mut exited = std::mem::zeroed();
            let mut kernel = std::mem::zeroed();
            let mut user = std::mem::zeroed();
            let read = GetProcessTimes(
                handle,
                &raw mut created,
                &raw mut exited,
                &raw mut kernel,
                &raw mut user,
            );
            CloseHandle(handle);
            (read != 0).then_some((kernel, user))
        };
        let (kernel, user) = times?;
        const TICKS_PER_MICRO: u64 = 10;
        Some(ticks(kernel).saturating_add(ticks(user)) / TICKS_PER_MICRO)
    }

    /// One `FILETIME` as the single number its two halves are.
    fn ticks(time: windows_sys::Win32::Foundation::FILETIME) -> u64 {
        (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
    }

    /// Every live process in the group `stamp` names, **or in one below it**.
    ///
    /// The "or below" is what makes a *run* answerable for its members: a member
    /// is grouped under its own scratch, `<run>/members/<name>`, and nothing is
    /// ever grouped under the run directory itself — so `cancel RUN --kill` has
    /// to reach a group recorded beneath the one it was given.
    pub fn stamped_for(stamp: &str) -> Vec<ProcessIdentity> {
        // Asking, not acting: a handle opened for `JOB_OBJECT_QUERY` alone
        // cannot terminate the tree it reports on, so a reader that is only ever
        // meant to enumerate cannot become a teardown by mistake.
        let mut found: Vec<ProcessIdentity> = groups_under(Path::new(stamp), JOB_OBJECT_QUERY)
            .into_iter()
            .flat_map(|job| pids_in(job.as_raw()))
            .filter_map(|pid| {
                let pid = i32::try_from(pid).ok()?;
                Some(ProcessIdentity {
                    pid,
                    start_token: start_token(pid)?,
                })
            })
            .collect();
        found.sort();
        found.dedup();
        found
    }

    /// Terminate every group at or below `scratch`, and say how many of the
    /// targets they held.
    ///
    /// One step rather than two: there is no signal for a process to decline
    /// here, so the ask-then-compel escalation POSIX makes has nothing to ask
    /// with. A group holding this process is left alone — a reap that took its
    /// own supervisor down would lose the report it exists to write.
    pub fn terminate(scratch: &Path, targets: &[ProcessIdentity]) -> usize {
        let own = std::process::id();
        let mut ended = 0;
        for job in groups_under(scratch, JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE) {
            let pids = pids_in(job.as_raw());
            if pids.contains(&own) {
                continue;
            }
            // SAFETY: `job` is a live job handle this frame owns; failure is
            // reported through the return value and there is nothing to do
            // about a job that has already gone.
            if unsafe { TerminateJobObject(job.as_raw(), 1) } != 0 {
                ended += targets
                    .iter()
                    .filter(|identity| {
                        u32::try_from(identity.pid).is_ok_and(|pid| pids.contains(&pid))
                    })
                    .count();
            }
        }
        ended
    }

    /// A job object is the kernel's own answer to "is anything still using
    /// this?" being asked of a *lock*, and `LockFileEx` is that answer for the
    /// lock itself — so acquiring one is evidence a sweeper may act on.
    pub const LOCK_PROVES_OWNERSHIP: bool = true;

    /// One member's process tree, as the job object holding it.
    #[derive(Debug)]
    pub struct Group {
        /// The job every process in the tree belongs to. Closing it terminates
        /// whatever is left, because the job is `KILL_ON_JOB_CLOSE`.
        job: Job,
        /// Where the job's name is recorded, removed with the group so a name
        /// that no longer resolves is not left behind for a sweeper to open.
        record: PathBuf,
    }

    impl Group {
        /// Create the job `scratch` names and record it there.
        pub fn open(scratch: &Path) -> Result<Self, Error> {
            let name = wide(&job_name(scratch));
            // SAFETY: `name` is a live, null-terminated wide string for the
            // duration of the call; failure is reported with a null handle.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), name.as_ptr()) };
            let job = Job::own(job).ok_or_else(|| {
                Error::InvalidConfig(format!(
                    "cannot create the job object for {}: {}",
                    scratch.display(),
                    std::io::Error::last_os_error()
                ))
            })?;
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` is a live, fully initialised structure of exactly
            // the size passed alongside it, and the class named is the one it is.
            let set = unsafe {
                SetInformationJobObject(
                    job.as_raw(),
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&limits).cast(),
                    u32::try_from(std::mem::size_of_val(&limits)).unwrap_or(u32::MAX),
                )
            };
            if set == 0 {
                return Err(Error::InvalidConfig(format!(
                    "cannot bind the job object for {} to its launcher: {}",
                    scratch.display(),
                    std::io::Error::last_os_error()
                )));
            }
            let record = scratch.join(OWNER_JOB_FILE);
            std::fs::write(&record, job_name(scratch)).map_err(|err| {
                Error::InvalidConfig(format!("cannot write {}: {err}", record.display()))
            })?;
            Ok(Self { job, record })
        }

        /// Ask for the child suspended, so it cannot run before it is in the
        /// job.
        ///
        /// Suspended and then assigned, rather than assigned after the fact: a
        /// child that runs for even an instant outside the job can start a
        /// descendant outside it too, and that descendant is the thing no cancel
        /// would ever reach.
        pub fn prepare(&self, command: &mut Command) -> std::io::Result<()> {
            command.creation_flags(CREATE_SUSPENDED);
            Ok(())
        }

        /// Put the suspended child in the job, leaving it suspended.
        ///
        /// A job assignment **nests**, which is what lets this be safe for a
        /// child somebody else spawned: a harness `oneharness_core` has already
        /// put in a job of its own joins this one as well, and either side's
        /// teardown ends it.
        //
        // llmlint: ignore-block[changed_behavior_has_e2e] there is no journey
        // for the failure arms because there is no input that reaches them: a
        // job this process created a moment ago refusing its own child, or a
        // suspended process with no resumable thread, are kernel failures rather
        // than anything a caller can ask for, and no seam this crate sanctions
        // fakes Win32. The reachable half — that a member which *is* grouped is
        // torn down whole, by a cancel, a watchdog, or its launcher dying — is
        // covered in tests/e2e/liveness.rs. What these arms decide is the
        // direction of an unreachable failure, and the callers make it the safe
        // one: a child that started but could not be put in the job is killed
        // rather than returned, because returning it would leave a paid harness
        // running that no cancel could ever find.
        pub fn join(&self, child: &Child) -> std::io::Result<()> {
            // SAFETY: the child handle is live for the borrow, and the job is
            // one this group owns; failure is reported through the return value.
            let assigned = unsafe {
                AssignProcessToJobObject(self.job.as_raw(), child.as_raw_handle() as HANDLE)
            };
            if assigned == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        /// Let a child this group's own [`Group::spawn`] started run.
        pub fn start(&self, child: &Child) -> std::io::Result<()> {
            resume(child.id())
        }
        // llmlint: ignore-end[changed_behavior_has_e2e]

        /// End this group's whole tree.
        pub fn terminate(&self, _scratch: &Path) -> usize {
            let pids = pids_in(self.job.as_raw());
            // SAFETY: the job is one this group owns; failure is reported
            // through the return value.
            if unsafe { TerminateJobObject(self.job.as_raw(), 1) } == 0 {
                return 0;
            }
            pids.len()
        }
    }

    impl Drop for Group {
        fn drop(&mut self) {
            // The handle closing is what terminates the stragglers, so the
            // record has to go first: a name still on disk after the job behind
            // it is gone is one a sweeper would open and find nothing in.
            let _ = std::fs::remove_file(&self.record);
        }
    }

    /// The name of the job object a scratch directory's tree belongs to.
    ///
    /// Derived from the path rather than minted, so two processes reach the same
    /// job without having to agree on anything but the directory — and digested
    /// rather than spelled, because a path carries separators an object name
    /// cannot and runs past the length one may be. `Local\` scopes it to this
    /// logon session, which is as far as a run and a `cancel` for it ever are
    /// from each other.
    fn job_name(scratch: &Path) -> String {
        let digest = Sha256::digest(scratch.display().to_string().as_bytes());
        format!("Local\\{}{digest:x}", super::SCRATCH_PREFIX)
    }

    /// One string as the null-terminated wide buffer the Win32 API takes.
    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Every group recorded at or below `root`, opened for `access` and no more.
    ///
    /// The rights are the caller's to name because they are not the same:
    /// enumerating a tree and ending one are different acts, and a handle that
    /// could do both is one an enumeration could end a member's tree through.
    ///
    /// A record naming a job nothing holds open any more simply does not open,
    /// which is the same answer as an empty group: that tree is gone.
    ///
    /// The name a group is opened under is **derived from the directory**, never
    /// taken from the file. `owner.job` is a file in a scratch directory — a
    /// trust boundary like any other, and one whose content would otherwise
    /// decide what `TerminateJobObject` is aimed at, so a record somebody else
    /// wrote would point a `cancel` at any job object this user can open. What
    /// the file is for is saying *that* a group was recorded here and letting an
    /// operator read which one; what it may not do is choose. A record that does
    /// not match the name this directory computes is not this crate's, and is
    /// skipped rather than opened.
    fn groups_under(root: &Path, access: u32) -> Vec<Job> {
        let mut found = Vec::new();
        let mut walking = vec![(root.to_path_buf(), 0usize)];
        while let Some((dir, depth)) = walking.pop() {
            let expected = job_name(&dir);
            if std::fs::read_to_string(dir.join(OWNER_JOB_FILE))
                .is_ok_and(|recorded| recorded.trim() == expected)
            {
                let name = wide(&expected);
                // SAFETY: `name` is a live, null-terminated wide string for the
                // duration of the call; failure is reported with a null handle.
                let job = unsafe { OpenJobObjectW(access, 0, name.as_ptr()) };
                found.extend(Job::own(job));
            }
            if depth >= RECORD_DEPTH {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    walking.push((entry.path(), depth + 1));
                }
            }
        }
        found
    }

    /// Every process id one job holds.
    fn pids_in(job: HANDLE) -> Vec<u32> {
        // Room for a member's whole tree in one call; the loop below grows to
        // whatever the kernel says it actually needs.
        let mut capacity = 64usize;
        for _ in 0..8 {
            // A `usize` buffer rather than a byte one, because the structure
            // this is read back as is pointer-aligned and a `Vec<u8>` is not.
            let mut buffer: Vec<usize> = vec![0; 2 + capacity];
            let bytes = u32::try_from(std::mem::size_of_val(buffer.as_slice())).unwrap_or(u32::MAX);
            let list = buffer
                .as_mut_ptr()
                .cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
            // SAFETY: `list` points at `bytes` bytes of live, zeroed, correctly
            // aligned memory this frame owns, and the class named is the one the
            // buffer is read back as.
            let queried = unsafe {
                QueryInformationJobObject(
                    job,
                    JobObjectBasicProcessIdList,
                    list.cast(),
                    bytes,
                    std::ptr::null_mut(),
                )
            };
            // SAFETY: the call fills the header even when it reports the buffer
            // was too small, which is what the retry below reads.
            let (assigned, listed) = unsafe {
                (
                    (*list).NumberOfAssignedProcesses as usize,
                    (*list).NumberOfProcessIdsInList as usize,
                )
            };
            // SAFETY: `listed` is the count the kernel wrote, which is bounded
            // by the capacity it was given.
            let pids = unsafe {
                std::slice::from_raw_parts(
                    (&raw const (*list).ProcessIdList).cast::<usize>(),
                    listed,
                )
            };
            let pids: Vec<u32> = pids
                .iter()
                .filter_map(|pid| u32::try_from(*pid).ok())
                .collect();
            if queried != 0 {
                return pids;
            }
            // SAFETY: reading the thread-local last error borrows nothing.
            if unsafe { GetLastError() } != ERROR_MORE_DATA {
                return Vec::new();
            }
            capacity = assigned.max(capacity * 2) + 8;
        }
        Vec::new()
    }

    /// Let a suspended process run.
    ///
    /// A process created suspended has exactly one thread, but it is reached
    /// through the thread table rather than through the [`Child`] — std does not
    /// keep the thread handle `CreateProcess` returned, and the alternative is
    /// not spawning through std at all.
    fn resume(pid: u32) -> std::io::Result<()> {
        // A snapshot of every thread on the host races the host: a thread table
        // that changed while it was being copied answers `ERROR_BAD_LENGTH`,
        // which is "ask again" rather than a failure. This process starts many
        // members at once, which is exactly when that happens.
        let mut snapshot = INVALID_HANDLE_VALUE;
        for _ in 0..16 {
            // SAFETY: the snapshot is a kernel handle this frame owns; failure
            // is reported with `INVALID_HANDLE_VALUE`.
            snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
            if snapshot != INVALID_HANDLE_VALUE {
                break;
            }
        }
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = u32::try_from(std::mem::size_of::<THREADENTRY32>()).unwrap_or(u32::MAX);
        let mut resumed = false;
        // SAFETY: `entry` is a live structure of the size it declares, and the
        // walk stops when the enumerator says there is nothing more.
        unsafe {
            let mut more = Thread32First(snapshot, &raw mut entry) != 0;
            while more {
                if entry.th32OwnerProcessID == pid {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() {
                        resumed |= ResumeThread(thread) != u32::MAX;
                        CloseHandle(thread);
                    }
                }
                more = Thread32Next(snapshot, &raw mut entry) != 0;
            }
            CloseHandle(snapshot);
        }
        if resumed {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "the member started suspended and could not be resumed (pid {pid})"
            )))
        }
    }

    /// One job object handle, closed when it goes out of scope.
    ///
    /// Closing is not bookkeeping here: the job is `KILL_ON_JOB_CLOSE`, so the
    /// last handle going away is what ends the tree.
    #[derive(Debug)]
    struct Job(OwnedHandle);

    impl Job {
        /// Take ownership of a handle a Win32 call returned, if it returned one.
        fn own(handle: HANDLE) -> Option<Self> {
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return None;
            }
            // SAFETY: the handle came straight from a call that returns
            // ownership of it, and nothing else holds it.
            Some(Self(unsafe { OwnedHandle::from_raw_handle(handle.cast()) }))
        }

        fn as_raw(&self) -> HANDLE {
            self.0.as_raw_handle().cast::<c_void>()
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::fs::File;

    use super::ProcessIdentity;

    /// Without a kernel lock there is no answer to "is anything still using
    /// this?", so the claim degrades to the directory's own existence and a
    /// sweeper is told to keep it.
    pub fn try_lock_exclusive(_file: &File) -> bool {
        true
    }

    /// No start token to be had here, so every identity carries `0` and none is
    /// ever reported live — which retains a directory rather than removing one
    /// still in use, the safe direction.
    pub fn start_token(_pid: i32) -> Option<u64> {
        None
    }

    /// No way to enumerate a member's tree here, so reaping falls back to the
    /// child the supervisor holds directly.
    pub fn stamped_for(_stamp: &str) -> Vec<ProcessIdentity> {
        Vec::new()
    }

    /// No CPU accounting either, so a member's silence is judged by its stream
    /// alone.
    pub fn consumed(_pid: i32) -> Option<u64> {
        None
    }

    /// Nothing is torn down through an unproven handle.
    pub fn terminate(_scratch: &std::path::Path, _targets: &[ProcessIdentity]) -> usize {
        0
    }

    /// Taking the lock here proves nothing: [`try_lock_exclusive`] always
    /// grants it, so a sweeper reading that as "nobody holds this" would
    /// reclaim a directory a live run is working in. Ownership degrades to the
    /// directory's own existence, and a sweeper is told to keep it.
    pub const LOCK_PROVES_OWNERSHIP: bool = false;

    /// Nothing groups a process tree here, so a group is the child alone.
    #[derive(Debug)]
    pub struct Group;

    impl Group {
        /// Nothing to open.
        pub fn open(_scratch: &std::path::Path) -> Result<Self, crate::error::Error> {
            Ok(Self)
        }

        /// Nothing to configure: there is no group to put the child in.
        pub fn prepare(&self, _command: &mut std::process::Command) -> std::io::Result<()> {
            Ok(())
        }

        /// Nothing to join either, for the same reason.
        pub fn join(&self, _child: &std::process::Child) -> std::io::Result<()> {
            Ok(())
        }

        /// Nothing was suspended, so nothing has to be started.
        pub fn start(&self, _child: &std::process::Child) -> std::io::Result<()> {
            Ok(())
        }

        /// Nothing provable to terminate.
        pub fn terminate(&self, _scratch: &std::path::Path) -> usize {
            0
        }
    }
}

pub use platform::stamped_for;
use platform::{try_lock_exclusive, LOCK_PROVES_OWNERSHIP};

/// This process's own identity.
///
/// Written once rather than per platform: what differs between them is only how
/// a start token is read. A platform that cannot read one leaves the `0` below,
/// which no real token ever equals — so an identity taken there is never
/// mistaken for a live process.
#[must_use]
pub fn own_identity() -> ProcessIdentity {
    // A pid past `i32::MAX` is reachable on Windows and would wrap to a negative
    // number, which [`recorded_identity`] rejects on the way back in. `0` names
    // no process on either platform, so such a run is judged by its lock alone
    // rather than by a number that would read as somebody else's.
    let pid = i32::try_from(std::process::id()).unwrap_or(0);
    ProcessIdentity {
        pid,
        start_token: platform::start_token(pid).unwrap_or(0),
    }
}

/// Whether `identity` still names the process it was taken from.
///
/// A platform that cannot answer for a token answers `None` here, which is never
/// equal to a recorded token — so an identity is reported live only where it can
/// be *proven* live, and a sweeper on such a platform falls back to the lock,
/// which is the independent half of the same proof.
#[must_use]
pub fn is_live(identity: ProcessIdentity) -> bool {
    platform::start_token(identity.pid).is_some_and(|token| token == identity.start_token)
}

/// One observation of the work under a scratch: how much CPU every live process
/// stamped for it has been charged between them, in microseconds.
///
/// Compared, never read. Two observations are a *rate* — see
/// [`worked`](Self::worked) — which is the whole of what [`work`] is
/// for.
///
/// One number rather than a process-by-process record, because the question is
/// about the tree rather than about any process in it, and a second signal is a
/// second thing to be sure of. A process that starts, or ends, is not tracked as
/// membership — it registers here as the CPU it was charged arriving in the
/// total or leaving it.
///
/// Opaque on purpose, and that is the invariant rather than a preference: a
/// comparison is only meaningful between two readings taken the same way of the
/// same stamp, and a caller that could name its own number could make an idle
/// tree compare as a busy one — the direction that spares a wedged member
/// forever. [`work`] is the only way to obtain one.
///
/// Not comparable for equality, and that is deliberate too: "the number moved"
/// was the rule once, and it is the one this replaces.
#[derive(Debug, Clone, Copy)]
pub struct Work(u64);

/// The share of one core a member's tree has to be charged, over the window
/// actually observed, before it counts as working — as a percentage.
///
/// Public because `docs/contract.md` states it and `tests/contract.rs` gates the
/// two against each other; this is the declaration, the document quotes it, and
/// neither can move without the other.
///
/// A rate rather than "the reading moved", because *moved* is a question about
/// the platform's accounting resolution — ten-millisecond ticks on Linux,
/// nanoseconds on macOS — which the same parked harness answers differently on
/// each. Coarsening the finer counter until they agreed was a calibration, and a
/// constant that makes a parked harness look idle on a quiet runner is one a
/// loaded runner can exceed.
///
/// The populations a rate separates are four orders of magnitude apart on every
/// platform. Measured here over ten seconds each: a parked process is charged
/// 0 µs/s; one waking every half-second — a harness with a heartbeat, the
/// noisiest thing a wedged tree holds — 121 µs/s, 0.012% of a core; a spin loop
/// 987,678 µs/s, 98.8%. One percent sits eighty times above the first pair and a
/// hundred times below the second, and load closes neither margin: a process
/// that is not running is not charged, and a spinning one would need a hundred
/// runnable threads per core to fall this far.
//
// llmlint: ignore-block[changed_behavior_has_e2e] the populations either side of
// this number have real journeys; the boundary itself has none, because holding a
// process at it for a whole bound is a verdict that flips with runner load — the
// flakiness this rate removes. It is covered either side in
// `member::tests::a_tree_is_judged_by_how_fast_it_is_charged_cpu`.
pub const WORKING_PERCENT_OF_A_CORE: u64 = 1;
// llmlint: ignore-end[changed_behavior_has_e2e]

impl Work {
    /// Whether the tree was working between this observation and `later`, taken
    /// `over` a window of wall-clock time.
    ///
    /// The comparison is a rate — CPU charged against wall time elapsed — held
    /// against [`WORKING_PERCENT_OF_A_CORE`]. It is the *magnitude* of the change
    /// rather than its sign, because a process leaving the tree takes its whole
    /// lifetime's CPU out of the total: a member whose child just finished a
    /// second of work is a member that was working, and a total that fell says
    /// so as plainly as one that rose.
    ///
    /// A window of nothing is no evidence of anything, and answers `false` —
    /// which condemns rather than spares. Every caller's window is at least one
    /// probe interval wide.
    #[must_use]
    pub fn worked(self, later: Self, over: Duration) -> bool {
        let charged = self.0.abs_diff(later.0);
        let window = u64::try_from(over.as_micros()).unwrap_or(u64::MAX);
        charged > window * WORKING_PERCENT_OF_A_CORE / 100
    }

    /// A reading this crate's own tests supply, so the rule can be driven over a
    /// sequence of observations without a real process to take them from.
    ///
    /// `#[cfg(test)]` is what keeps the invariant above true where it matters:
    /// no shipped build has a second way to make one, so no caller can hand the
    /// watchdog a number of its own choosing.
    #[cfg(test)]
    pub(crate) const fn of_micros(micros: u64) -> Self {
        Self(micros)
    }
}

/// Observe the work under `scratch` as it stands right now.
///
/// The evidence is the same the reap and the sweep rest on — the stamp the
/// kernel fixed at `exec`, or the job object that stands in for it — so this
/// reaches a descendant whose parent has already exited, which is exactly the
/// process a supervisor's own view of its child cannot see.
///
/// An empty answer means nothing is running under this scratch, which is a
/// member with no work to be doing rather than one whose work could not be
/// examined: a platform that cannot enumerate a tree answers empty *every* time,
/// so it never reads as a change.
#[must_use]
pub fn work(scratch: &Path) -> Work {
    Work(
        stamped_for(&scratch.display().to_string())
            .into_iter()
            // A process this build cannot read counts as nothing, which is the
            // same answer a platform with no accounting at all gives: it makes a
            // tree look *less* busy, and the direction that errs is the one that
            // still condemns a wedged member.
            .filter_map(|identity| platform::consumed(identity.pid))
            .fold(0u64, u64::saturating_add),
    )
}

/// Terminate every live process this scratch still holds — the member's own
/// process and everything it started, whether or not their parents are still
/// alive.
///
/// Returns how many were signalled. What "signalled" means is the platform's:
/// POSIX asks with `SIGTERM`, waits out a short grace period, and `SIGKILL`s
/// whatever declined; Windows has no signal to decline, so its job object is
/// terminated in one step. Both end with the same guarantee — nothing stamped
/// for this scratch is still running — which is the property the rule is for.
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
    platform::terminate(scratch, &targets)
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
        #[cfg(any(unix, windows))]
        assert!(retained.contains("still locked by its owner"), "{retained}");
        #[cfg(not(any(unix, windows)))]
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
    ///
    /// Held wherever the kernel backs the lock — an `flock` on POSIX, a
    /// `LockFileEx` byte range on Windows — and both refuse a second acquisition
    /// even from the process already holding the first, which is what makes this
    /// assertable in one process.
    #[cfg(any(unix, windows))]
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
    #[cfg(any(unix, windows))]
    #[test]
    fn a_live_recorded_identity_pins_the_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        let own = own_identity();

        // Each case gets its own directory rather than rewriting one lock file
        // in place. A kernel lock is held by an *open handle*, and rewriting a
        // file this process has already opened and closed makes the second
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

    /// A lock file is external input, so a record this crate would never have
    /// written is refused at the boundary rather than carried into a platform
    /// call.
    ///
    /// The direction matters as much as the refusal. A rejected record leaves the
    /// kernel lock as the only proof, and here that lock is free — so these are
    /// reclaimable, which is the answer for a directory nothing can be shown to
    /// be using. What must never happen is the other reading: a non-positive pid
    /// treated as an identity, which is `kill`'s whole-process-group address on
    /// POSIX and widens into an unrelated live pid on Windows.
    #[test]
    fn a_lock_recording_no_identity_this_crate_would_write_is_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        for (name, record) in [
            ("negative", "-1 1\n"),
            ("group", "-4242 1\n"),
            ("zero", "0 1\n"),
            // The shape, not just the number: `claim` writes two fields, so a
            // third is a file somebody else wrote.
            ("trailing", "4242 99 someone-elses-record\n"),
            ("second-line", "4242 99\n7 7\n"),
        ] {
            let path = root.path().join(name);
            std::fs::create_dir_all(&path).expect("mkdir");
            std::fs::write(path.join(OWNER_LOCK_FILE), record).expect("write");
            assert_eq!(
                recorded_identity(&path.join(OWNER_LOCK_FILE)),
                None,
                "{record:?}"
            );
            assert_eq!(reclaimable(&path), Ok(()), "{record:?}");
        }

        // And the boundary is a check, not a blanket refusal: the record `claim`
        // actually writes still reads back as the identity it names.
        let path = root.path().join("positive");
        std::fs::create_dir_all(&path).expect("mkdir");
        std::fs::write(path.join(OWNER_LOCK_FILE), "4242 99\n").expect("write");
        assert_eq!(
            recorded_identity(&path.join(OWNER_LOCK_FILE)),
            Some(ProcessIdentity {
                pid: 4242,
                start_token: 99
            })
        );
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
    #[cfg(any(unix, windows))]
    #[test]
    fn an_identity_answers_for_the_process_it_was_taken_from() {
        assert!(is_live(own_identity()));
        assert!(!is_live(ProcessIdentity {
            pid: -1,
            start_token: 0
        }));
    }
}
