//! Role-addressed notes: an update to one party's task, carried from outside a
//! run into the two-party conversation that is having it.
//!
//! `interrupt` redirects the turn an operator addresses, and that is the whole of
//! what it can do. It is why a note has only ever reached one of the two parties:
//! [`crate::judge`] records a controllable turn for the **agent** side alone,
//! because that is the only side onejudge asks oneharness to open one for. A
//! ruling delivered that way reaches the worker and never the judge, and a judge
//! reviewing against a task that never mentioned it contradicts the ruling it was
//! never shown.
//!
//! # The shapes are onejudge's, not this crate's
//!
//! [`Addressee`], [`Note`], [`Accepted`] and [`Undelivered`] are **re-exports** of
//! [`onejudge::note`], which is where the approved delivery-seam contract puts
//! them: onejudge owns the two-party conversation, so it owns the note that
//! enters one. Nothing about them is declared here, and that is deliberate — a
//! second declaration is a shape that drifts, and a note that satisfies the copy
//! is still refused by the conversation it was written for.
//!
//! # The routing is onejudge's too
//!
//! Which side of a member is live is a fact only the engine driving it has, so
//! this crate does not decide it. The engine's own end of the channel
//! ([`onejudge::note::NoteInbox`]) goes onto the [`onejudge::cli::Plan`]
//! [`crate::judge`] drives, and what it does with a note is the contract's:
//!
//! * **The worker's turn is live** — that turn is reopened carrying the note,
//!   *before* the supervisor is consulted, so the judge receives the note
//!   together with the worker's response to it rather than ahead of one.
//!   [`Accepted::Interrupted`] naming [`Party::Worker`].
//! * **The supervisor's turn is live** — its decision is re-taken with the note in
//!   hand, and the note rides that response to the worker.
//!   [`Accepted::Interrupted`] naming [`Party::Supervisor`], or
//!   [`Accepted::JudgedWith`] when the re-taken decision is completion: the work
//!   was passed with the note in hand and there was no next worker turn to
//!   deliver it into.
//! * **Between turns** — the next turn to open takes it. [`Accepted::Queued`],
//!   answered once it is really in that turn's transcript.
//! * **The conversation is over** — [`Undelivered`], naming which. Never a silent
//!   acceptance: a note taken into a member nothing will read it out of looks, to
//!   the caller, exactly like one that landed.
//!
//! # What is this crate's: getting the note there
//!
//! A note is offered by a *different process* from the one running the member —
//! `oneagentgraph`'s own API, against a run's state directory — and
//! [`onejudge::note::Notes`] is an in-process handle. The transport between the
//! two is this module's whole job: a [`Spool`] the member binds in its own
//! scratch, and a `Courier` on a thread of the member's process that carries
//! what lands there into [`onejudge::note::Notes::send`] and writes the
//! conversation's own answer back.
//!
//! On a thread of its own, and that is load-bearing: `send` blocks until the
//! note's disposition is known — for the supervisor, until its re-taken decision
//! comes back — so servicing the spool from the supervision loop would stall the
//! watchdogs behind a judge invocation.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::event::{Emitter, EventKind, TurnInterrupted};
use crate::member::as_payload;

pub use onejudge::note::{
    Accepted, Addressee, Criterion, DeliveredNote, Note, NoteRefused, NoteText, Party, Undelivered,
};

/// The directory a two-party member binds inside its own scratch to receive
/// notes for the conversation's life — a sibling of
/// [`crate::control::CONTROL_FILE`].
pub const NOTES_DIR: &str = "notes";

/// The shape this build writes into every spooled note and every answer.
///
/// Read back by a differently-versioned build exactly as
/// [`crate::control::CONTROL_SCHEMA_VERSION`] is, and refused by number for the
/// same reason: a document from a build that knew more is not guessed at.
pub const NOTE_SCHEMA_VERSION: u32 = 1;

/// The file a member writes once its conversation can take no more notes, so one
/// arriving after that is refused rather than spooled to nobody.
const ENDED_FILE: &str = "ended.json";

/// How long [`submit`] waits for the member's own answer before reporting that
/// the conversation never took the note.
///
/// A live delivery is what spends the time here: the courier hands the note over
/// and waits for the conversation to move, which for a supervisor-side delivery
/// is a whole judge invocation. Thirty seconds is past that and still an answer
/// rather than a hang.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);

/// How often [`submit`] looks for the answer, and how often a [`Courier`] with
/// nothing to do looks for a note.
const ANSWER_POLL: Duration = Duration::from_millis(50);

/// What one [`submit`] answered: the conversation took the note, or it did not.
///
/// The two are not interchangeable and neither is an exit code: a caller reads
/// the [`Accepted`] to know *when* the addressee sees it, and the [`Undelivered`]
/// to know it never will.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteDelivery {
    /// The conversation took it.
    Accepted(Accepted),
    /// It was not delivered.
    Undelivered(Undelivered),
}

/// One note as it sits in a member's spool, waiting to be taken.
///
/// [`Note`] carries its own validation across `serde` — its text is a
/// [`NoteText`] and its criterion a [`Criterion`], both checked in the conversion
/// that deserializes them — so a hand-written document that would not have built
/// a note does not read back as one either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Spooled {
    /// The shape this document was written under — see [`NOTE_SCHEMA_VERSION`].
    schema_version: u32,
    /// The note itself.
    note: Note,
}

/// The answer a member writes back for one spooled note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Answered {
    /// The shape this document was written under — see [`NOTE_SCHEMA_VERSION`].
    schema_version: u32,
    /// What became of the note.
    delivery: WireDelivery,
}

// The three types below are the wire mirror `onejudge::note::Undelivered`'s own
// documentation calls for: *"this enum is mirrored one-to-one by the transports
// that carry a note in from outside the process, and a variant renamed on one
// side of that mapping is a variant silently dropped on the other."* This spool
// is one of those transports, and onejudge's `Accepted` and `Undelivered` are not
// `Serialize` — deliberately, since what crosses a process boundary is a
// transport's decision rather than theirs. Mapped in both directions immediately
// below, exhaustively, so a variant added upstream fails this build instead of
// being dropped in transit.
/// [`Accepted`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireAccepted {
    /// [`Accepted::Queued`].
    Queued,
    /// [`Accepted::Interrupted`].
    Interrupted {
        /// The party whose turn it reached.
        party: Party,
    },
    /// [`Accepted::JudgedWith`].
    JudgedWith {
        /// The supervisor's completion reason.
        completion_reason: String,
    },
}

/// [`Undelivered`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireUndelivered {
    /// [`Undelivered::ConversationCompleted`].
    ConversationCompleted {
        /// The supervisor's completion reason.
        completion_reason: String,
    },
    /// [`Undelivered::MemberSettled`].
    MemberSettled {
        /// How the conversation ended.
        outcome: String,
    },
    /// [`Undelivered::NoConversation`].
    NoConversation {
        /// What became of the channel instead.
        reason: String,
    },
}

/// [`NoteDelivery`] on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireDelivery {
    /// [`NoteDelivery::Accepted`].
    Accepted(WireAccepted),
    /// [`NoteDelivery::Undelivered`].
    Undelivered(WireUndelivered),
}

impl From<&Accepted> for WireAccepted {
    fn from(accepted: &Accepted) -> Self {
        match accepted {
            Accepted::Queued => Self::Queued,
            Accepted::Interrupted { party } => Self::Interrupted { party: *party },
            Accepted::JudgedWith { completion_reason } => Self::JudgedWith {
                completion_reason: completion_reason.clone(),
            },
        }
    }
}

impl From<WireAccepted> for Accepted {
    fn from(wire: WireAccepted) -> Self {
        match wire {
            WireAccepted::Queued => Self::Queued,
            WireAccepted::Interrupted { party } => Self::Interrupted { party },
            WireAccepted::JudgedWith { completion_reason } => {
                Self::JudgedWith { completion_reason }
            }
        }
    }
}

impl From<&Undelivered> for WireUndelivered {
    fn from(undelivered: &Undelivered) -> Self {
        match undelivered {
            Undelivered::ConversationCompleted { completion_reason } => {
                Self::ConversationCompleted {
                    completion_reason: completion_reason.clone(),
                }
            }
            Undelivered::MemberSettled { outcome } => Self::MemberSettled {
                outcome: outcome.clone(),
            },
            Undelivered::NoConversation { reason } => Self::NoConversation {
                reason: reason.clone(),
            },
        }
    }
}

impl From<WireUndelivered> for Undelivered {
    fn from(wire: WireUndelivered) -> Self {
        match wire {
            WireUndelivered::ConversationCompleted { completion_reason } => {
                Self::ConversationCompleted { completion_reason }
            }
            WireUndelivered::MemberSettled { outcome } => Self::MemberSettled { outcome },
            WireUndelivered::NoConversation { reason } => Self::NoConversation { reason },
        }
    }
}

impl From<&NoteDelivery> for WireDelivery {
    fn from(delivery: &NoteDelivery) -> Self {
        match delivery {
            NoteDelivery::Accepted(accepted) => Self::Accepted(accepted.into()),
            NoteDelivery::Undelivered(undelivered) => Self::Undelivered(undelivered.into()),
        }
    }
}

impl From<WireDelivery> for NoteDelivery {
    fn from(wire: WireDelivery) -> Self {
        match wire {
            WireDelivery::Accepted(accepted) => Self::Accepted(accepted.into()),
            WireDelivery::Undelivered(undelivered) => Self::Undelivered(undelivered.into()),
        }
    }
}

/// What a member wrote when it stopped taking notes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ended {
    /// The shape this document was written under — see [`NOTE_SCHEMA_VERSION`].
    schema_version: u32,
    /// The refusal every later note gets, in the conversation's own words.
    refusal: WireUndelivered,
}

/// Where one member receives notes: the directory its own thread binds for the
/// conversation's life, and the two ends that meet in it.
///
/// A directory rather than a socket, and that is the one deviation from the
/// approved contract's wording: the record still names the endpoint by path, and
/// the two ends still meet nowhere else, but a member of this crate runs on
/// Windows too — where a unix domain socket is exactly why
/// [`crate::control`] already reports *no controllable turn* — and a note seam
/// that existed on one platform only would be a delivery an operator could not
/// rely on.
#[derive(Debug, Clone)]
pub struct Spool {
    dir: PathBuf,
}

impl Spool {
    /// The spool one member's notes are exchanged in, whether or not it exists.
    #[must_use]
    pub fn at(scratch: &Path) -> Self {
        Self {
            dir: scratch.join(NOTES_DIR),
        }
    }

    /// Create it, the way a member binding its endpoint does.
    ///
    /// [`None`] when the directory cannot be made, which is a member with no note
    /// endpoint — reported to a caller as one rather than failing a run that is
    /// otherwise fine, exactly as a control record that could not be written is.
    #[must_use]
    pub fn bind(scratch: &Path) -> Option<Self> {
        let spool = Self::at(scratch);
        std::fs::create_dir_all(&spool.dir).ok()?;
        Some(spool)
    }

    /// Where it is, which is what a member's control record names.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// The refusal this member recorded, if it is no longer taking notes.
    fn ended(&self) -> Option<Undelivered> {
        let raw = std::fs::read_to_string(self.dir.join(ENDED_FILE)).ok()?;
        let ended: Ended = serde_json::from_str(&raw).ok()?;
        (ended.schema_version == NOTE_SCHEMA_VERSION).then(|| ended.refusal.into())
    }

    /// Record that this member takes no more notes, so one arriving after it is
    /// refused rather than spooled to nobody.
    fn end(&self, refusal: &Undelivered) {
        let document = Ended {
            schema_version: NOTE_SCHEMA_VERSION,
            refusal: refusal.into(),
        };
        if let Ok(rendered) = serde_json::to_string(&document) {
            let _ = std::fs::write(self.dir.join(ENDED_FILE), rendered);
        }
    }

    /// The file note `id` is offered in.
    fn request(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.note.json"))
    }

    /// Where a member writes what became of note `id`.
    fn answer(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.answer.json"))
    }

    /// Offer `note` to the member under `id`, which is what its answer will be
    /// written beside.
    ///
    /// Written beside its final name and renamed onto it, so a member servicing
    /// the spool never reads half a document.
    fn offer(&self, id: &str, note: &Note) -> std::io::Result<()> {
        let document = Spooled {
            schema_version: NOTE_SCHEMA_VERSION,
            note: note.clone(),
        };
        let rendered = serde_json::to_string(&document).map_err(std::io::Error::other)?;
        let staging = self.dir.join(format!("{id}.staging"));
        std::fs::write(&staging, rendered)?;
        std::fs::rename(&staging, self.request(id))
    }

    /// Every note offered since the last take, with the request file removed —
    /// so one note is taken once however often the member services its spool.
    fn take(&self) -> Vec<(String, Note)> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut taken: Vec<(String, Note)> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let id = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".note.json"))?
                    .to_string();
                let raw = std::fs::read_to_string(&path).ok()?;
                // A document on disk is external input whatever wrote it, and
                // every check on it happens before the file is touched: removing
                // it *is* acting on it, so a boundary check that has already
                // deleted its own input is not a check. The *note* validates
                // itself on the way out of `serde` — blank text and an unusable
                // criterion are both refused by the conversions
                // `onejudge::note` deserializes through — so what is left to
                // check here is the version.
                let spooled: Spooled = serde_json::from_str(&raw).ok()?;
                if spooled.schema_version != NOTE_SCHEMA_VERSION {
                    return None;
                }
                // Past every check, and the only place this directory is written
                // by the taking side: one note is taken once however often the
                // member services its spool, while a document this build cannot
                // read is left exactly where it is — the skew that produced it
                // survives for whoever comes to look.
                let _ = std::fs::remove_file(&path);
                Some((id, spooled.note))
            })
            .collect();
        // Deterministic, and in the order the ids were minted: an id carries the
        // instant it was made, so two notes offered in one tick reach the turn in
        // the order the operator sent them.
        taken.sort_by(|left, right| left.0.cmp(&right.0));
        taken
    }

    /// Write what became of note `id`.
    fn settle(&self, id: &str, delivery: &NoteDelivery) {
        let document = Answered {
            schema_version: NOTE_SCHEMA_VERSION,
            delivery: delivery.into(),
        };
        if let Ok(rendered) = serde_json::to_string(&document) {
            let staging = self.dir.join(format!("{id}.answering"));
            if std::fs::write(&staging, rendered).is_ok() {
                let _ = std::fs::rename(&staging, self.answer(id));
            }
        }
    }

    /// Read back what a member answered for `id`, if it has.
    fn answered(&self, id: &str) -> Option<NoteDelivery> {
        let raw = std::fs::read_to_string(self.answer(id)).ok()?;
        let answered: Answered = serde_json::from_str(&raw).ok()?;
        (answered.schema_version == NOTE_SCHEMA_VERSION).then(|| answered.delivery.into())
    }
}

/// Hand `note` to the conversation running in `scratch`, and answer what became
/// of it.
///
/// The caller's end of the seam: the note is offered in the member's own spool,
/// and the member's own courier thread hands it to the conversation's inbox. What
/// comes back is the conversation's own answer, not a guess made from outside.
///
/// # Errors
///
/// [`Undelivered`], for a note the conversation did not take: it had already
/// completed, it had ended, or it never answered at all.
pub fn submit(scratch: &Path, note: &Note) -> Result<Accepted, Undelivered> {
    submit_within(scratch, note, ANSWER_DEADLINE)
}

/// [`submit`], with the wait made a parameter so a test can drive the deadline
/// itself rather than sitting out the real one.
fn submit_within(scratch: &Path, note: &Note, deadline: Duration) -> Result<Accepted, Undelivered> {
    let spool = Spool::at(scratch);
    if !spool.dir.is_dir() {
        return Err(Undelivered::NoConversation {
            reason: "this member binds no note endpoint: only a two-party member's own thread \
                     does, and this run recorded none for it"
                .to_string(),
        });
    }
    // Before anything is offered, because a conversation that is over will never
    // service its spool again and a note left in it would be an acceptance
    // nothing reads.
    if let Some(refusal) = spool.ended() {
        return Err(refusal);
    }
    let id = mint();
    if let Err(err) = spool.offer(&id, note) {
        return Err(Undelivered::NoConversation {
            reason: format!(
                "the note could not be written into the member's spool at {}: {err}",
                spool.dir.display()
            ),
        });
    }
    let until = Instant::now() + deadline;
    loop {
        if let Some(delivery) = spool.answered(&id) {
            let _ = std::fs::remove_file(spool.answer(&id));
            return match delivery {
                NoteDelivery::Accepted(accepted) => Ok(accepted),
                NoteDelivery::Undelivered(undelivered) => Err(undelivered),
            };
        }
        // llmlint: ignore-block[changed_behavior_has_e2e] no journey can produce
        // this: it needs a member that bound its endpoint and then stopped
        // servicing it, which is a wedged process rather than anything a graph, a
        // task or a config can ask for — and the one seam this suite may fake is
        // the paid harness, which is below the thread that services the spool.
        // What it decides is the direction of a host failure, and it is the safe
        // one: the caller is told the note did not land rather than held forever.
        // `tests::a_member_that_never_takes_a_note_is_reported_rather_than_waited_on`
        // drives it against a real spool nothing is servicing.
        if Instant::now() >= until {
            // Taken back, so a member that wakes up later does not deliver a note
            // its caller has already been told was not delivered.
            let _ = std::fs::remove_file(spool.request(&id));
            return Err(Undelivered::NoConversation {
                reason: format!(
                    "the member's conversation did not take the note within {} seconds — it is \
                     not servicing its notes",
                    deadline.as_secs()
                ),
            });
        }
        // llmlint: ignore-end[changed_behavior_has_e2e]
        std::thread::sleep(ANSWER_POLL);
    }
}

/// An id no two notes share, ordered by the instant it was minted.
///
/// Three segments, and each carries one of those two properties. The clock leads,
/// so [`Spool::take`]'s lexicographic sort is offer order. The process id
/// separates two *processes* offering at once, which is the ordinary case: a note
/// arrives from outside the run.
///
/// The counter is what makes the first half of the sentence true rather than
/// likely. Two threads of one process may read the same nanosecond — the API
/// permits it, and `tests/e2e/note.rs` offers from a thread of its own — and two
/// notes sharing an id would share a spool file, so one of them would vanish
/// under the other with its caller waiting on an answer that never comes.
fn mint() -> String {
    /// Distinct per note within this process, whatever the clock's resolution.
    static MINTED: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    // Zero-padded to its full width, so the counter orders lexicographically for
    // every value it can take rather than up to its first carry.
    format!(
        "{now:039}-{}-{:020}",
        std::process::id(),
        MINTED.fetch_add(1, Ordering::Relaxed)
    )
}

/// What a member with **no conversation** is handed instead: the addressed role,
/// then the note's own text, unchanged.
///
/// A single-sided `kind: oneharness` member has one party and one lever, so a
/// note to it falls through to [`crate::control::interrupt`] — see
/// [`crate::control::note`]. There is no conversation layer to frame it, so the
/// frame is written here, and it says the one thing the two-party framing exists
/// to say: whose task this update belongs to.
#[must_use]
pub(crate) fn framed(note: &Note) -> String {
    format!(
        "— run note (addressed to: {}) —\n{}\n\n{}",
        note.addressee.as_str(),
        "The following update was delivered to this member's task; act on it.",
        note.text
    )
}

/// The member's end of the note seam: the thread that carries what its spool
/// receives into the conversation's own inbox, and writes the answer back.
///
/// On a thread of its own because [`onejudge::note::Notes::send`] blocks until
/// the note's disposition is known — for a supervisor-side delivery, until that
/// party's re-taken decision comes back. Servicing the spool from the supervision
/// loop would put a judge invocation between two heartbeats.
pub(crate) struct Courier {
    spool: Spool,
    notes: onejudge::note::Notes,
    stop: Arc<AtomicBool>,
    emitter: Emitter,
}

impl Courier {
    /// Open the courier for a member, and the [`Ending`] its supervisor closes it
    /// with.
    pub(crate) fn open(
        spool: Spool,
        notes: onejudge::note::Notes,
        emitter: &Emitter,
    ) -> (Self, Ending) {
        let stop = Arc::new(AtomicBool::new(false));
        let ending = Ending {
            spool: spool.clone(),
            stop: Arc::clone(&stop),
            emitter: emitter.clone(),
        };
        let courier = Self {
            spool,
            notes,
            stop,
            emitter: emitter.clone(),
        };
        (courier, ending)
    }

    /// Carry notes until this member's conversation is over. The thread body.
    pub(crate) fn serve(self) {
        while !self.stop.load(Ordering::SeqCst) {
            for (id, note) in self.spool.take() {
                // Blocks: the conversation is what decides, and for a note that
                // reaches the supervisor's live turn the decision *is* the answer.
                let delivery = match self.notes.send(note.clone()) {
                    Ok(accepted) => NoteDelivery::Accepted(accepted),
                    Err(undelivered) => NoteDelivery::Undelivered(undelivered),
                };
                publish(&self.emitter, &note, &delivery);
                self.spool.settle(&id, &delivery);
            }
            std::thread::sleep(ANSWER_POLL);
        }
    }
}

/// How a member's supervisor closes its note seam.
///
/// Both halves matter. The record is what refuses a note that arrives after this,
/// and the answers are what keep a note already in the spool from being an
/// acceptance nobody reads.
pub(crate) struct Ending {
    spool: Spool,
    stop: Arc<AtomicBool>,
    emitter: Emitter,
}

impl Ending {
    /// The directory this member bound, which is what its control record names.
    pub(crate) fn endpoint(&self) -> &Path {
        self.spool.path()
    }

    /// This member takes no more notes, and `refusal` is what every later one
    /// gets — the conversation's own reason, so a caller reads *completed* and
    /// *ended* apart rather than being told only that it was too late.
    pub(crate) fn end(&self, refusal: &Undelivered) {
        self.spool.end(refusal);
        self.stop.store(true, Ordering::SeqCst);
        // Anything the courier had not reached is answered here rather than left
        // for its caller to time out on.
        for (id, note) in self.spool.take() {
            let delivery = NoteDelivery::Undelivered(refusal.clone());
            publish(&self.emitter, &note, &delivery);
            self.spool.settle(&id, &delivery);
        }
    }
}

/// Publish what became of one note on the run's own stream.
///
/// A caller learns the disposition from its own [`submit`]; this is the *run's*
/// record of it, so an operator reading the journal sees a note arrive without
/// having sent it.
fn publish(emitter: &Emitter, note: &Note, delivery: &NoteDelivery) {
    let reason = match delivery {
        NoteDelivery::Accepted(_) => None,
        NoteDelivery::Undelivered(undelivered) => Some(undelivered.to_string()),
    };
    emitter.emit(
        EventKind::TurnInterrupted,
        as_payload(&TurnInterrupted {
            member: emitter.member().unwrap_or_default().to_string(),
            delivered: reason.is_none(),
            input_bytes: note.text.as_str().len() as u64,
            reason,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Labels;

    /// An emitter that writes nowhere, labelled for one member — what a courier
    /// publishes its deliveries on.
    fn emitter(member: &str) -> Emitter {
        Emitter::new("stream", Box::new(std::io::sink())).with_labels(Labels {
            member: Some(member.to_string()),
            ..Labels::default()
        })
    }

    fn note(addressee: Addressee) -> Note {
        Note::new(addressee, "the migration has to be reversible").expect("a note")
    }

    /// A note offered with **no turn open** is held for the next turn, and a note
    /// offered once the conversation can take no more is refused — both answered
    /// by the conversation itself, through this crate's transport.
    ///
    /// That round trip is what this crate owns here. The routing is onejudge's,
    /// and `tests/e2e/note.rs` drives the live-turn deliveries against a real
    /// conversation; what is proven here is that a note written into a member's
    /// spool by one process reaches `Notes::send` unchanged and that the answer
    /// `send` gives is what the caller reads, rather than anything this crate
    /// decided for itself.
    ///
    /// Both dispositions are driven against a **real** `onejudge` channel rather
    /// than a stand-in, in the two states a channel can be put into from outside
    /// the engine: one no turn has opened on yet, which is exactly *no live turn*
    /// — the note is accepted for the next turn to open — and one whose engine
    /// end is gone, which is the refusal a caller must not read as a delivery.
    #[test]
    fn a_note_with_no_live_turn_is_held_for_the_next_one_and_one_too_late_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::bind(dir.path()).expect("a spool");
        let (notes, inbox) = onejudge::note::Notes::channel();
        let (courier, _ending) = Courier::open(spool.clone(), notes, &emitter("worker"));
        std::thread::spawn(move || courier.serve());

        // No turn has opened, so there is nothing live to deliver into and the
        // note is held for the next turn that opens.
        assert_eq!(
            submit(dir.path(), &note(Addressee::Worker)),
            Ok(Accepted::Queued),
            "a note offered with no live turn was not held for the next one"
        );

        // And once nothing will read the channel again, a note is refused rather
        // than accepted into a member nobody will take it out of.
        drop(inbox);
        let refused = submit(dir.path(), &note(Addressee::Worker))
            .expect_err("a conversation nothing is running cannot take a note");
        assert!(
            matches!(&refused, Undelivered::NoConversation { reason }
                if reason.contains("dropped before any turn opened")),
            "the conversation's own refusal did not reach the caller: {refused:?}"
        );
        assert!(
            refused.to_string().contains("was not delivered"),
            "the refusal did not say the note was not delivered: {refused}"
        );
    }

    /// A member that takes no more notes refuses one rather than accepting it,
    /// and says which of the two terminal facts it is.
    ///
    /// Both are driven, because a caller acts on the difference: a conversation
    /// its supervisor *passed* needs no relaunch and the note is a follow-up,
    /// while one that merely ended may be worth starting again. Every arm's
    /// `Display` opens with the same words, so a caller that only prints it still
    /// learns the note did not land.
    #[test]
    fn a_member_that_stops_taking_notes_refuses_them_rather_than_accepting_one() {
        for refusal in [
            Undelivered::ConversationCompleted {
                completion_reason: "its supervisor judged the task complete".to_string(),
            },
            Undelivered::MemberSettled {
                outcome: "the member was condemned by its heartbeat watchdog".to_string(),
            },
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let spool = Spool::bind(dir.path()).expect("a spool");
            let ending = Ending {
                spool: spool.clone(),
                stop: Arc::new(AtomicBool::new(false)),
                emitter: emitter("worker"),
            };

            // In the spool when the conversation ends, and answered by it rather
            // than left for its caller to time out on.
            spool.offer("a", &note(Addressee::Worker)).expect("offered");
            ending.end(&refusal);
            assert_eq!(
                spool.answered("a"),
                Some(NoteDelivery::Undelivered(refusal.clone())),
                "a note already in the spool was not answered by the end of the conversation"
            );

            // And offered afterwards: refused before it is spooled at all,
            // because nothing will service it again.
            let refused = submit(dir.path(), &note(Addressee::Worker))
                .expect_err("a conversation that is over cannot take a note");
            assert_eq!(refused, refusal, "the refusal did not name what happened");
            assert!(
                refused.to_string().contains("was not delivered"),
                "the refusal did not say the note was not delivered: {refused}"
            );
        }
    }

    /// A member with no endpoint, and one that is not servicing the endpoint it
    /// has, are both reported rather than waited on forever.
    ///
    /// The second is the one that matters: a caller blocked on a member that has
    /// stopped answering is a caller that never learns its note did not land,
    /// which is the same silence the whole seam exists to replace. The request is
    /// taken back on the way out, so a member that wakes up later does not
    /// deliver a note its caller was already told about.
    #[test]
    fn a_member_that_never_takes_a_note_is_reported_rather_than_waited_on() {
        let dir = tempfile::tempdir().expect("tempdir");
        let absent = submit(dir.path(), &note(Addressee::Worker))
            .expect_err("a member with no endpoint cannot take a note");
        assert!(
            matches!(&absent, Undelivered::NoConversation { reason }
                if reason.contains("binds no note endpoint")),
            "{absent:?}"
        );

        let spool = Spool::bind(dir.path()).expect("a spool");
        let silent = submit_within(
            dir.path(),
            &note(Addressee::Worker),
            Duration::from_millis(10),
        )
        .expect_err("a member that services nothing cannot take a note");
        assert!(
            matches!(&silent, Undelivered::NoConversation { reason }
                if reason.contains("did not take the note")),
            "{silent:?}"
        );
        assert!(
            spool.take().is_empty(),
            "a note its caller was told about was left in the spool for a member to deliver later"
        );
    }

    /// One note is taken once, in the order it was offered, and a document this
    /// build did not write is left alone rather than acted on.
    #[test]
    fn the_spool_takes_each_note_once_and_ignores_what_it_did_not_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::bind(dir.path()).expect("a spool");
        for id in ["000-1", "001-1", "002-1"] {
            spool
                .offer(id, &Note::new(Addressee::Worker, id).expect("a note"))
                .expect("offered");
        }
        let taken = spool.take();
        assert_eq!(
            taken.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            ["000-1", "001-1", "002-1"],
            "the notes were not taken in the order they were offered"
        );
        assert!(spool.take().is_empty(), "a note was taken twice");

        // A note claiming a version this build did not write is not acted on: it
        // was written by a build that knew something this one does not, and
        // delivering it would be delivering a document of unknown shape.
        std::fs::write(
            spool.request("999-1"),
            format!(
                "{{\"schema_version\":{},\"note\":{{\"addressee\":\"worker\",\"text\":\"x\"}}}}",
                NOTE_SCHEMA_VERSION + 1
            ),
        )
        .expect("write");
        // And one whose *note* this build would not have built: `NoteText` refuses
        // blank text in the conversion it deserializes through, so a hand-written
        // document carrying one does not read back as a note at all.
        std::fs::write(
            spool.request("998-1"),
            format!(
                "{{\"schema_version\":{NOTE_SCHEMA_VERSION},\"note\":{{\"addressee\":\"worker\",\
                 \"text\":\"   \"}}}}"
            ),
        )
        .expect("write");
        std::fs::write(spool.request("997-1"), "not a note").expect("write");
        assert!(
            spool.take().is_empty(),
            "a note this build cannot read was acted on"
        );
        // Left alone means left on disk: removing it would be acting on it, and
        // would destroy the one record of the skew that wrote it.
        for id in ["999-1", "998-1", "997-1"] {
            assert!(
                spool.request(id).exists(),
                "a document this build cannot read was removed rather than left alone: {id}"
            );
        }
    }

    /// Every disposition the conversation can reach survives the wire the spool
    /// carries it over, unchanged.
    ///
    /// The mirror is the thing most likely to rot: `onejudge::note::Accepted` and
    /// `Undelivered` are not `Serialize`, so this crate maps them by hand, and a
    /// variant that lost a field in transit would answer a caller something the
    /// conversation never said. Round-tripped through the same `Spool::settle` /
    /// `Spool::answered` pair a member and its caller really use.
    #[test]
    fn every_disposition_survives_the_spool_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::bind(dir.path()).expect("a spool");
        let dispositions = [
            NoteDelivery::Accepted(Accepted::Queued),
            NoteDelivery::Accepted(Accepted::Interrupted {
                party: Party::Worker,
            }),
            NoteDelivery::Accepted(Accepted::Interrupted {
                party: Party::Supervisor,
            }),
            NoteDelivery::Accepted(Accepted::JudgedWith {
                completion_reason: "the supervisor passed it with the note in hand".to_string(),
            }),
            NoteDelivery::Undelivered(Undelivered::ConversationCompleted {
                completion_reason: "already answered completion".to_string(),
            }),
            NoteDelivery::Undelivered(Undelivered::MemberSettled {
                outcome: "condemned by its heartbeat watchdog".to_string(),
            }),
            NoteDelivery::Undelivered(Undelivered::NoConversation {
                reason: "nothing ever read this channel".to_string(),
            }),
        ];
        for (index, disposition) in dispositions.iter().enumerate() {
            let id = format!("{index}");
            spool.settle(&id, disposition);
            assert_eq!(
                spool.answered(&id).as_ref(),
                Some(disposition),
                "{disposition:?} did not survive the spool"
            );
        }
    }

    /// A member with no conversation is handed the addressed role too.
    ///
    /// A single-sided member has one party and one lever, so its note falls
    /// through to `interrupt` — there is no conversation layer to frame it, and
    /// the frame written here says the one thing the two-party framing exists to
    /// say.
    #[test]
    fn a_note_to_a_member_with_no_conversation_still_names_its_addressee() {
        for addressee in [Addressee::Worker, Addressee::Supervisor, Addressee::Both] {
            let framed = framed(&note(addressee));
            assert!(
                framed.contains(&format!("addressed to: {}", addressee.as_str())),
                "{addressee:?} did not name itself: {framed}"
            );
            assert!(
                framed.contains("the migration has to be reversible"),
                "{addressee:?} did not carry the text: {framed}"
            );
        }
    }
}
