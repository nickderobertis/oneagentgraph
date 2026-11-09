//! A run's clocks beyond a member's own `schedule`: what each member's clocks
//! reset on, and the clocks that fire a two-party member's scheduled notes.
//!
//! A member's launch is built before the run starts and is the same value
//! `validate` builds, so nothing that exists only while a run is live rides it.
//! What a clock needs at run time — its `reset_on` subscription, and the seam its
//! scheduled notes are offered through — is kept here instead, keyed by the
//! member's scratch directory: the one value every part of a member already
//! derives its run-time state from, the way a hold derives its signal
//! directory. The run registers a member's clocks before its first wave and
//! forgets them once the graph has settled.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::config::ScheduledNote;
use crate::event::{CronReset, Emitter, WithdrawnReason};
use crate::member::as_payload;
use crate::note::{Fired, ScheduledNotes};
use crate::resets::ResetOn;

/// How often a scheduled note's clock looks at what moves it — the run's own
/// clock tick.
const TICK: Duration = Duration::from_millis(100);

/// One member's run-time clock state.
#[derive(Default)]
pub(crate) struct MemberClocks {
    /// The member's own `schedule.reset_on` subscription, when it declares one:
    /// read by the run's clock for a single-sided member or a deferred first
    /// turn, and by the hold between a paced conversation's turns.
    pub(crate) schedule_reset: Option<ResetOn>,
    /// Where a two-party member's scheduled notes are offered into its
    /// conversation, when it declares any.
    pub(crate) notes: Option<Arc<ScheduledNotes>>,
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<MemberClocks>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<MemberClocks>>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// The clocks of the member whose scratch is `scratch`, while its run is live.
pub(crate) fn of(scratch: &Path) -> Option<Arc<MemberClocks>> {
    registry()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(scratch)
        .cloned()
}

/// A run's registered members, forgotten together when it is dropped — on the
/// way out of every path, a refusal included.
pub(crate) struct Registered(Vec<PathBuf>);

impl Registered {
    pub(crate) fn new() -> Self {
        Self(Vec::new())
    }

    /// Register `clocks` under `scratch`, replacing what a previous run in the
    /// same scratch left — a scratch is one run's, so there is none in practice.
    pub(crate) fn register(&mut self, scratch: &Path, clocks: MemberClocks) -> Arc<MemberClocks> {
        let clocks = Arc::new(clocks);
        registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(scratch.to_path_buf(), Arc::clone(&clocks));
        self.0.push(scratch.to_path_buf());
        clocks
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        let mut registry = registry().lock().unwrap_or_else(PoisonError::into_inner);
        for scratch in &self.0 {
            registry.remove(scratch);
        }
    }
}

/// What one scheduled note's clock is given.
pub(crate) struct NoteClock {
    /// The note as declared.
    pub(crate) note: ScheduledNote,
    /// Where it is offered.
    pub(crate) seam: Arc<ScheduledNotes>,
    /// Its own `reset_on` subscription, when it declares one.
    pub(crate) reset_on: Option<ResetOn>,
    /// The member's emitter.
    pub(crate) emitter: Emitter,
    /// The run's signal directory.
    pub(crate) signals: PathBuf,
    /// The member's name, for its own `cancel`.
    pub(crate) member: crate::run::MemberName,
    /// Set once the graph is settling: every clock stops.
    pub(crate) over: Arc<AtomicBool>,
    /// How this clock restarts on a match: published first, counted after.
    pub(crate) restart: crate::resets::Restart,
    /// The suite's lever for *when* this clock starts counting — see
    /// [`NOTE_CLOCK_FROM`].
    #[cfg(feature = "test-doubles")]
    pub(crate) from: Option<crate::member::FixtureGate>,
}

/// The marker a journey puts in its member's **task** to have each of that
/// member's scheduled-note clocks start counting only once the gate it names
/// exists, rather than when the member's wave is reached.
///
/// A note's clock is wall-clock time, and the conversation it offers into
/// moves at whatever pace its turns take: a journey that has to see a firing
/// land *during a hold*, or *during a live turn*, cannot get there by choosing
/// spans and hoping the host keeps up. The gate makes the firing the journey's
/// to place. Compiled only under `test-doubles`, with the prefix every fixture
/// of this crate's carries; in the task for `crate::judge`'s
/// `HOLD_BETWEEN_TURNS` reason.
#[cfg(feature = "test-doubles")]
pub(crate) const NOTE_CLOCK_FROM: &str = "oneagentgraph-fixture:note-clock-from=";

/// Start one scheduled note's clock, from now.
///
/// It counts `start_after`, then `every`, by wall clock and independently of
/// the conversation's turns; a firing is [`ScheduledNotes::fire`]'s to offer,
/// hold or refuse. A `reset_on` match restarts the span pending — the first
/// span before the first firing, `every` after — and withdraws the untaken
/// copy. It stops when the run settles, on the run's `stop` or the member's
/// own `cancel`, and after the first firing that found the conversation over.
// llmlint: ignore-block[changed_behavior_has_e2e] the `None` this answers is the
// host refusing this process one more thread, which no graph, task or config
// asks for and no seam this crate sanctions fakes — the refusal every thread arm
// in `crate::judge` and `crate::note` records on the same terms. A clock started
// in a later wave cannot refuse a graph already running, so the note is not
// offered rather than the run being taken down over it. The reachable half — a
// clock that runs — is every journey in `tests/e2e/scheduled_notes.rs`.
pub(crate) fn spawn_note_clock(clock: NoteClock) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("note-{}", clock.note.id))
        .spawn(move || run_note_clock(&clock))
        .ok()
}
// llmlint: ignore-end[changed_behavior_has_e2e]

fn run_note_clock(clock: &NoteClock) {
    let id = crate::note::ScheduledNoteId::of(&clock.note);
    let Ok(note) = ScheduledNotes::note(clock.note.addressee, &clock.note.text) else {
        return;
    };
    let stop = clock.signals.join("stop");
    let own_stop = clock
        .signals
        .join(format!("{}.stop", clock.member.as_str()));
    // Measured against `elapsed` rather than as a deadline, for the reason
    // `crate::run::cron` gives.
    let mut span = Duration::from_secs(clock.note.first_after());
    #[cfg(feature = "test-doubles")]
    if let Some(gate) = &clock.from {
        let _ = std::fs::write(&gate.entered, "entered");
        while !gate.gate.exists() {
            std::thread::sleep(TICK);
            if clock.over.load(Ordering::SeqCst) || stop.exists() || own_stop.exists() {
                return;
            }
        }
    }
    let mut counting_since = Instant::now();
    loop {
        std::thread::sleep(TICK);
        if clock.over.load(Ordering::SeqCst) || stop.exists() || own_stop.exists() {
            return;
        }
        if let Some(cause) = clock.reset_on.as_ref().and_then(ResetOn::take) {
            counting_since = clock.restart.announce(
                &clock.emitter,
                as_payload(&CronReset {
                    scheduled_note: Some(id.as_str().to_string()),
                    cause: Some(cause),
                }),
            );
            clock
                .seam
                .withdraw_untaken(&id, WithdrawnReason::Reset, &clock.emitter);
            continue;
        }
        if counting_since.elapsed() < span {
            continue;
        }
        if clock.seam.fire(&id, &note, &clock.emitter) == Fired::Undelivered {
            return;
        }
        span = Duration::from_secs(clock.note.every);
        counting_since = Instant::now();
    }
}
