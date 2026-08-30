//! Role-addressed notes: an update to one party's task, handed to a live
//! two-party conversation.
//!
//! `interrupt` redirects the turn an operator addresses. That is the whole of
//! what it can do, and it is why a note has only ever reached one of the two
//! parties: [`crate::judge`] records a controllable turn for the **agent** side
//! alone, because that is the only side onejudge asks oneharness to open one for.
//! A ruling delivered that way reaches the worker and never the judge, and a
//! judge reviewing against a task that never mentioned it contradicts the ruling
//! it was never shown.
//!
//! A note is the other shape of the same delivery, and three things distinguish
//! it from an interrupt:
//!
//! * It is **addressed to a role** ([`Addressee`]), so the party that receives it
//!   knows whose task it updates rather than reading every update as its own
//!   next instruction.
//! * It is **routed to whichever side of the member is live**, read off the
//!   member's own conversation as onejudge reports it, rather than aimed at one
//!   fixed socket.
//! * A note that **cannot** be delivered is an [`Undelivered`] error naming that
//!   it was not delivered and why, rather than an acceptance nothing will read.
//!
//! # Where this surface comes from
//!
//! [`Addressee`], [`Note`], [`Accepted`] and [`Undelivered`] are the approved
//! delivery-seam contract's own shapes, field for field. That contract puts the
//! first three in a `note` module of **onejudge** — the crate that owns the
//! two-party conversation — and mirrors [`Undelivered`] here. They are declared
//! here as well because the published onejudge this crate links exposes no such
//! module yet: a graph resolves from crates.io alone, so this crate builds
//! against the onejudge surface that exists. Nothing about the shapes is this
//! crate's own invention, which is what keeps the adoption a re-export rather
//! than a translation.
//!
//! # What this crate can hand a live conversation, and what it cannot
//!
//! The one place text enters a running onejudge conversation from outside is the
//! controllable turn the agent side opens. So a note this crate delivers reaches
//! the conversation there:
//!
//! * **The agent's turn is live** — the note goes into that turn, framed with its
//!   addressee, through the same `oneharness interrupt` an
//!   [`crate::control::interrupt`] uses. [`Accepted::Interrupted`].
//! * **The judge's turn is live, or the member is between turns** — there is no
//!   out-of-band lever on the judge side (onejudge opens one for the agent only),
//!   so the note is held and delivered into the **next** agent turn: it arrives
//!   with the instruction that turn answers, which is the judge's own response.
//!   [`Accepted::Queued`].
//! * **The conversation is over, or the member has settled** — [`Undelivered`],
//!   naming which.
//!
//! The half that is *not* here is the half the approved contract puts in
//! onejudge: a note reaching the supervisor's own effective task, `notes` and
//! completion criteria. That is composed inside onejudge's engine loop from
//! values a plan hands it before the run starts, and no seam the published
//! library exposes lets an embedder add to it mid-run. A supervisor-addressed
//! note is therefore routed and framed here and answered
//! [`Undelivered::NoConversation`] against a onejudge with no inbox to take it,
//! rather than smuggled to the judge through the worker's reply — which would
//! make delivery depend on an agent choosing to quote it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::event::{Emitter, EventKind, Party, TurnInterrupted};
use crate::member::as_payload;

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

/// The file a member writes once its conversation is over, so a note arriving
/// after it is refused rather than spooled to nobody.
const COMPLETED_FILE: &str = "completed.json";

/// How long [`submit`] waits for the member's own answer before reporting that
/// the conversation never took the note.
///
/// The member services its spool once per [`crate::member::HEARTBEAT_INTERVAL`]
/// and answers every case on the tick it picks a note up — the one that spends
/// longer is a live delivery, which waits on an `oneharness interrupt` process.
/// Thirty seconds is far past both and still an answer rather than a hang.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);

/// How often [`submit`] looks for the answer.
const ANSWER_POLL: Duration = Duration::from_millis(50);

// llmlint: ignore-block[contracts_have_one_source_or_a_drift_gate] there is no
// second source for these four shapes to drift from, and no way to build one:
// the approved contract puts them in a `note` module of onejudge, and **no
// published onejudge has that module** — 0.6.2 is the newest and its `lib.rs`
// declares `cli`, `command`, `control`, `engine`, `error`, `oneharness`,
// `provider`, `report`, `sdk_schema`, `skill`, `spawn`, `split`, `stream`,
// `telemetry`, `transcript` and `usage`, and nothing else. A gate has to compare
// against something that exists; a test asserting against a module that does not
// compile is not a drift gate but a build failure, and a copy of the contract's
// prose committed here to diff against would be the mirror this rule exists to
// prevent — it drifts, and a shape that passes it is still not the one onejudge
// ships. What holds the two together instead is the *adoption*: when that
// release lands, these declarations become `pub use onejudge::note::{…}` and the
// duplicate is deleted rather than reconciled, which is why every field here is
// spelled exactly as the contract spells it. `control.json`, whose source **is**
// this crate, is gated by `tests/record.rs` and its committed golden.
/// Who a note is for.
///
/// No default, and stated per note: an update whose addressee had to be guessed
/// is exactly the failure this type exists to remove — a judge that reads the
/// worker's amendment as its own instruction takes on the worker's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Addressee {
    /// The party doing the work.
    Worker,
    /// The party judging it.
    Supervisor,
    /// Both, which is what an amendment to the task is.
    Both,
}

impl Addressee {
    /// The token this addressee is written and read as, which is also the word
    /// the receiving party is shown.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worker => "worker",
            Self::Supervisor => "supervisor",
            Self::Both => "both",
        }
    }

    /// Whether this note is addressed to the worker — the omitted value in a
    /// caller that writes notes for one party only.
    #[must_use]
    pub fn is_worker(self) -> bool {
        matches!(self, Self::Worker)
    }

    /// Whether the party doing the work is one of this note's addressees.
    #[must_use]
    pub fn reaches_worker(self) -> bool {
        matches!(self, Self::Worker | Self::Both)
    }

    /// Whether the party judging the work is one of this note's addressees.
    #[must_use]
    pub fn reaches_supervisor(self) -> bool {
        matches!(self, Self::Supervisor | Self::Both)
    }
}

/// One update to a party's task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    /// Who it is for. Required — see [`Addressee`].
    pub addressee: Addressee,
    // llmlint: ignore-block[invalid_states_unrepresentable] a `NoteText` newtype
    // making a blank one unrepresentable is the right shape and is not this
    // crate's to choose: the approved delivery-seam contract spells this field
    // `pub text: String`, and this crate is the consuming half of that seam — a
    // newtype here would refuse the very value the producing half hands over, and
    // would stop the adoption being a re-export the day onejudge publishes the
    // module. What holds the invariant instead is [`Note::check`], applied at
    // every boundary a note crosses rather than once where an ideal value was
    // built, because a `Note` arrives *deserialized* as often as it arrives from
    // a constructor and a newtype's `Deserialize` would have to be written by
    // hand to hold anything a bare `String`'s does not.
    /// The update itself, carried to the receiving party verbatim.
    pub text: String,
    // llmlint: ignore-end[invalid_states_unrepresentable]
    // llmlint: ignore-block[invalid_states_unrepresentable] the name, the type
    // and the default are the approved delivery-seam contract's own, spelled
    // `pub binds: bool` with `default false`, and this crate is the *consuming*
    // half of that seam. A two-variant enum here would read better and would be
    // this crate deciding a shared surface unilaterally — the one thing the
    // contract that produced this field says never to do — and it would stop the
    // adoption being a re-export the day onejudge publishes the module. A third
    // mode is a proposal to that contract's owner, and lands as a field there
    // first.
    /// Whether the update binds the task: an amendment the work is judged
    /// against, rather than context for it. Carried through unchanged, because
    /// what it binds — the supervisor's completion criteria — is composed inside
    /// the conversation layer.
    #[serde(default)]
    pub binds: bool,
    // llmlint: ignore-end[invalid_states_unrepresentable]
}

impl Note {
    /// A note addressed to `addressee`, carrying `text`.
    ///
    /// # Errors
    ///
    /// [`crate::error::Error::InvalidConfig`] when `text` is blank: a note with
    /// nothing in it is an update nobody can act on, and delivering one would
    /// spend a turn on a redirection that says nothing.
    pub fn new(addressee: Addressee, text: impl Into<String>) -> Result<Self, crate::error::Error> {
        let note = Self {
            addressee,
            text: text.into(),
            binds: false,
        };
        note.check()?;
        Ok(note)
    }

    /// Whether this note is one a party could act on.
    ///
    /// A constructor is not enough to hold that: the fields are public because
    /// the approved contract makes them so, and a `Note` also arrives
    /// *deserialized* — off the spool, or out of a caller's own JSON — where no
    /// constructor ran. So this is checked again at every boundary a note
    /// crosses rather than once where the ideal one was built:
    /// [`crate::control::note()`] before it routes anything, [`submit`] before it
    /// offers one, and the member's own spool on the way back off disk.
    ///
    /// # Errors
    ///
    /// [`crate::error::Error::InvalidConfig`] when the text is blank: an update
    /// with nothing in it is not one a party can act on, and delivering it would
    /// spend a turn on a redirection that says nothing.
    pub fn check(&self) -> Result<(), crate::error::Error> {
        if self.text.trim().is_empty() {
            return Err(crate::error::Error::InvalidConfig(
                "a note needs text: an update with nothing in it is not one a party can act on"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// The same note, binding the task rather than adding context to it.
    #[must_use]
    pub fn binding(mut self) -> Self {
        self.binds = true;
        self
    }

    /// What the receiving party is handed: the addressed role, then the note's
    /// own text, unchanged.
    ///
    /// The header is the whole point of a *role-addressed* note — a party that
    /// cannot tell whose task an update belongs to reads every one of them as its
    /// own next instruction — so it names the addressee in
    /// [`Addressee::as_str`]'s own token and says, for a note the receiving party
    /// is not the addressee of, that it is not an instruction to it.
    #[must_use]
    pub fn framed(&self) -> String {
        let addressee = self.addressee.as_str();
        let binding = if self.binds {
            " It amends the task, so the work is judged against it."
        } else {
            ""
        };
        let closing = match self.addressee {
            Addressee::Worker => {
                "The following update was delivered to the worker's task; act on it.".to_string()
            }
            Addressee::Supervisor => "The following update was delivered to the worker's task; \
                                      judge whether it was done. It is not an instruction to you."
                .to_string(),
            Addressee::Both => "The following update was delivered to the worker's task; act on \
                                it, and it is judged against."
                .to_string(),
        };
        format!(
            "— run note (addressed to: {addressee}) —\n{closing}{binding}\n\n{}",
            self.text
        )
    }
}

/// What became of a note the conversation took.
///
/// Three answers rather than a boolean, because a caller acts on the difference:
/// a note that redirected the turn in flight has already changed what the member
/// is doing, one that is queued will change the turn after this one, and one the
/// supervisor already passed the work with changes nothing and needs no relaunch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accepted {
    /// It reaches the addressee at the next boundary.
    Queued,
    /// It was delivered into the live agent turn.
    Interrupted,
    /// The judge passed the work with the note in hand.
    JudgedWith {
        /// What the supervisor said when it passed the work.
        completion_reason: String,
    },
}

/// A note that was **not** delivered, and why.
///
/// An error rather than a deferral, and deliberately so: the failure this
/// replaces is a note accepted into a member nothing would ever read it out of,
/// which reads to the caller exactly like one that landed. A caller holding one
/// of these chooses — relaunch the member, amend it for a later dispatch, or
/// record it as a follow-up — and can only choose because it was told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Undelivered {
    /// The conversation reached its completion decision before the note did.
    ConversationCompleted {
        /// What the supervisor said when it ended the conversation.
        completion_reason: String,
    },
    /// The member has settled: its turns are over.
    MemberSettled {
        /// What the run recorded for it.
        outcome: String,
    },
    /// There was no conversation to hand the note to, and this is why.
    NoConversation {
        /// The reason, in the words the caller reports.
        reason: String,
    },
}

impl std::fmt::Display for Undelivered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Every arm opens with the same four words, because that is the fact the
        // caller acts on and it must not depend on which arm it read.
        match self {
            Self::ConversationCompleted { completion_reason } => write!(
                f,
                "the note was not delivered: the member's supervisor had already answered \
                 completion ({completion_reason}), so nothing will read it"
            ),
            Self::MemberSettled { outcome } => write!(
                f,
                "the note was not delivered: the member has already settled ({outcome}), so its \
                 turns are over"
            ),
            Self::NoConversation { reason } => {
                write!(f, "the note was not delivered: {reason}")
            }
        }
    }
}

impl std::error::Error for Undelivered {}

// llmlint: ignore-end[contracts_have_one_source_or_a_drift_gate]

/// What one [`submit`] answered: the conversation took the note, or it did not.
///
/// The two are not interchangeable and neither is an exit code: a caller reads
/// the [`Accepted`] to know *when* the addressee sees it, and the [`Undelivered`]
/// to know it never will.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteDelivery {
    /// The conversation took it.
    Accepted(Accepted),
    /// It was not delivered.
    Undelivered(Undelivered),
}

/// One note as it sits in a member's spool, waiting to be taken.
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
    delivery: NoteDelivery,
}

/// What a member wrote when its conversation ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completed {
    /// The shape this document was written under — see [`NOTE_SCHEMA_VERSION`].
    schema_version: u32,
    /// What the conversation ended on, in the supervisor's own words where it
    /// gave any.
    completion_reason: String,
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

    /// The completion the member recorded, if its conversation is over.
    #[must_use]
    pub fn completion(&self) -> Option<String> {
        let raw = std::fs::read_to_string(self.dir.join(COMPLETED_FILE)).ok()?;
        let completed: Completed = serde_json::from_str(&raw).ok()?;
        (completed.schema_version == NOTE_SCHEMA_VERSION).then_some(completed.completion_reason)
    }

    /// Record that this member's conversation is over, so a note arriving after
    /// it is refused rather than spooled to nobody.
    fn complete(&self, completion_reason: &str) {
        let document = Completed {
            schema_version: NOTE_SCHEMA_VERSION,
            completion_reason: completion_reason.to_string(),
        };
        if let Ok(rendered) = serde_json::to_string(&document) {
            let _ = std::fs::write(self.dir.join(COMPLETED_FILE), rendered);
        }
    }

    /// The request file one note is offered in, and the answer file beside it.
    fn request(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.note.json"))
    }

    /// Where a member writes what became of note `id`.
    fn answer(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.answer.json"))
    }

    /// Offer `note` to the member and return the id it is answered under.
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
                let _ = std::fs::remove_file(&path);
                let spooled: Spooled = serde_json::from_str(&raw).ok()?;
                // The version *and* the note itself: a document on disk is
                // external input whatever wrote it, and a hand-written one can
                // carry a shape [`Note::new`] would have refused. Dropped rather
                // than answered, exactly as a document this build cannot read is
                // — [`submit`] refuses a blank note before it ever reaches the
                // spool, so anything blank here was put there by something else.
                (spooled.schema_version == NOTE_SCHEMA_VERSION && spooled.note.check().is_ok())
                    .then_some((id, spooled.note))
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
            delivery: delivery.clone(),
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
        (answered.schema_version == NOTE_SCHEMA_VERSION).then_some(answered.delivery)
    }
}

/// Hand `note` to the conversation running in `scratch`, and answer what became
/// of it.
///
/// The caller's end of the seam: the note is offered in the member's own spool
/// and the member — which is the only thing that knows which side of its
/// conversation is live — decides. What comes back is the member's own answer,
/// not a guess made from outside.
///
/// # Errors
///
/// [`Undelivered`], for a note the conversation did not take: it had already
/// completed, or it never answered at all.
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
    // Before anything is offered, because a `Note` reaching here need not have
    // come from [`Note::new`] — see [`Note::check`].
    if let Err(err) = note.check() {
        return Err(Undelivered::NoConversation {
            reason: err.to_string(),
        });
    }
    // Before anything is offered, because a conversation that is over will never
    // service its spool again and a note left in it would be an acceptance
    // nothing reads.
    // llmlint: ignore-block[changed_behavior_has_e2e] this arm is the window the
    // whole seam exists to close, and closing it is what makes it unreachable
    // from outside: a member records its completion and then settles, and
    // `crate::control::note` answers a settled member `MemberSettled` before it
    // ever reaches here. The gap between the two is the member's own settle path
    // — microseconds, and not something a journey can hold open without a second
    // faked seam. `tests::a_completed_conversation_refuses_a_note_rather_than_accepting_it`
    // drives the real `Router::complete` and the real `submit` across it.
    if let Some(completion_reason) = spool.completion() {
        return Err(Undelivered::ConversationCompleted { completion_reason });
    }
    // llmlint: ignore-end[changed_behavior_has_e2e]
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
fn mint() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{now:039}-{}", std::process::id())
}

/// Which side of a member's conversation is taking a turn right now.
///
/// Written by the member's own engine thread as onejudge opens and closes each
/// turn, read by the thread servicing its notes — so the routing rests on the
/// conversation's own structure rather than on this crate's guess about it.
#[derive(Debug)]
pub(crate) struct LiveTurn {
    /// `0` between turns; otherwise the party, one more than its discriminant.
    side: AtomicU8,
}

/// [`LiveTurn::side`]'s value between turns.
const BETWEEN_TURNS: u8 = 0;
/// [`LiveTurn::side`]'s value while the party doing the work is taking a turn.
const AGENT_LIVE: u8 = 1;
/// [`LiveTurn::side`]'s value while any other party is.
const OTHER_LIVE: u8 = 2;

impl Default for LiveTurn {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveTurn {
    /// A member that has not opened a turn yet.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            side: AtomicU8::new(BETWEEN_TURNS),
        }
    }

    /// `party` opened a turn.
    pub(crate) fn opened(&self, party: Party) {
        let side = if matches!(party, Party::Assistant) {
            AGENT_LIVE
        } else {
            OTHER_LIVE
        };
        self.side.store(side, Ordering::SeqCst);
    }

    /// The turn that was open closed.
    pub(crate) fn closed(&self) {
        self.side.store(BETWEEN_TURNS, Ordering::SeqCst);
    }

    /// Whether the party doing the work is taking a turn right now, which is the
    /// one case a note can be delivered into.
    #[must_use]
    pub(crate) fn agent_is_live(&self) -> bool {
        self.side.load(Ordering::SeqCst) == AGENT_LIVE
    }
}

/// The member's end of the note seam: what it does with the notes its spool
/// receives.
///
/// Lives on the member's supervision thread rather than its engine thread, for
/// the reason every other delivery in this crate does: handing a note to the
/// conversation means running an `oneharness interrupt` process, and doing that
/// from the sink would stop the engine from reporting the turn it is delivering
/// into.
pub(crate) struct Router {
    spool: Spool,
    live: std::sync::Arc<LiveTurn>,
    address: crate::control::Address,
    oneharness_bin: String,
    /// Notes answered [`Accepted::Queued`], waiting for the next agent turn.
    held: Vec<Note>,
}

impl Router {
    /// The router for a member whose agent turns are addressed at `address`.
    #[must_use]
    pub(crate) fn new(
        spool: Spool,
        live: std::sync::Arc<LiveTurn>,
        address: crate::control::Address,
        oneharness_bin: String,
    ) -> Self {
        Self {
            spool,
            live,
            address,
            oneharness_bin,
            held: Vec::new(),
        }
    }

    /// One service tick: take what arrived, route each note to the side that is
    /// live, and deliver whatever has been waiting for an agent turn.
    pub(crate) fn service(&mut self, emitter: &Emitter) {
        for (id, note) in self.spool.take() {
            let delivery = self.route(&note, emitter);
            self.spool.settle(&id, &delivery);
        }
        if self.live.agent_is_live() && !self.held.is_empty() {
            for note in std::mem::take(&mut self.held) {
                self.deliver(&note, emitter);
            }
        }
    }

    /// Where one arriving note goes.
    fn route(&mut self, note: &Note, emitter: &Emitter) -> NoteDelivery {
        if let Some(completion_reason) = self.spool.completion() {
            return NoteDelivery::Undelivered(Undelivered::ConversationCompleted {
                completion_reason,
            });
        }
        if !note.addressee.reaches_worker() {
            // The judge's own copy is the conversation layer's half of this seam:
            // it reaches the supervisor through the effective task and the
            // completion criteria the engine composes, and the published onejudge
            // this crate links exposes no way to add to either mid-run. Refused
            // rather than delivered to the worker under a supervisor's frame,
            // which would be a note the addressee never sees and the wrong party
            // acting on.
            return NoteDelivery::Undelivered(Undelivered::NoConversation {
                reason: format!(
                    "a note addressed to the {} reaches it through the conversation's own \
                     effective task and completion criteria, and the onejudge this build links \
                     exposes no inbox for them: only the worker's side of a two-party member \
                     opens a turn a note can be delivered into",
                    note.addressee.as_str()
                ),
            });
        }
        if self.live.agent_is_live() {
            return self.deliver(note, emitter);
        }
        // Held rather than delivered now: the judge is mid-decision, or nobody is
        // taking a turn, and the next agent turn is opened by the response this
        // note will arrive with.
        self.held.push(note.clone());
        NoteDelivery::Accepted(Accepted::Queued)
    }

    /// Hand one note to the live agent turn, and publish what happened.
    fn deliver(&self, note: &Note, emitter: &Emitter) -> NoteDelivery {
        let framed = note.framed();
        let delivery = crate::control::deliver(&self.oneharness_bin, &self.address, Some(&framed));
        let (answer, reason) = match delivery {
            crate::control::Delivery::Delivered => {
                (NoteDelivery::Accepted(Accepted::Interrupted), None)
            }
            crate::control::Delivery::NoTurn(reason)
            | crate::control::Delivery::Failed(reason)
            | crate::control::Delivery::Invalid(reason) => (
                NoteDelivery::Undelivered(Undelivered::NoConversation {
                    reason: reason.clone(),
                }),
                Some(reason),
            ),
        };
        emitter.emit(
            EventKind::TurnInterrupted,
            as_payload(&TurnInterrupted {
                member: emitter.member().unwrap_or_default().to_string(),
                delivered: reason.is_none(),
                input_bytes: framed.len() as u64,
                reason,
            }),
        );
        answer
    }

    /// The conversation is over: record it, and answer everything still waiting.
    ///
    /// Both halves matter. The record is what refuses a note that arrives after
    /// this, and the answers are what keeps a note already in the spool from
    /// being an acceptance nobody reads — including one this router answered
    /// [`Accepted::Queued`] and never got an agent turn to deliver into, which is
    /// published as the undelivered note it is.
    pub(crate) fn complete(&mut self, completion_reason: &str, emitter: &Emitter) {
        self.spool.complete(completion_reason);
        for (id, _) in self.spool.take() {
            self.spool.settle(
                &id,
                &NoteDelivery::Undelivered(Undelivered::ConversationCompleted {
                    completion_reason: completion_reason.to_string(),
                }),
            );
        }
        for note in std::mem::take(&mut self.held) {
            let reason = format!(
                "the conversation completed ({completion_reason}) before the worker took another \
                 turn, so the queued note was never delivered"
            );
            emitter.emit(
                EventKind::TurnInterrupted,
                as_payload(&TurnInterrupted {
                    member: emitter.member().unwrap_or_default().to_string(),
                    delivered: false,
                    input_bytes: note.framed().len() as u64,
                    reason: Some(reason),
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Labels;

    /// An emitter that writes nowhere, labelled for one member — what a router
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

    /// A router whose deliveries go to a binary that is not there, so the
    /// *routing* is what the assertion is about rather than a socket.
    fn router(spool: Spool, live: &std::sync::Arc<LiveTurn>) -> Router {
        Router::new(
            spool,
            std::sync::Arc::clone(live),
            crate::control::Address {
                session: "node-scope-1-worker-skill".into(),
                session_dir: None,
                cwd: "/work/repo".into(),
            },
            "oneagentgraph-no-such-oneharness".to_string(),
        )
    }

    /// The receiving party is told whose task the update belongs to, and the
    /// note's own text arrives unchanged.
    ///
    /// The header is the whole of what makes a note *addressed*: without it a
    /// judge handed an update to the worker's task reads it as its own next
    /// instruction and takes on the worker's job.
    #[test]
    fn a_note_names_its_addressee_and_carries_its_text_unchanged() {
        for addressee in [Addressee::Worker, Addressee::Supervisor, Addressee::Both] {
            let framed = note(addressee).framed();
            assert!(
                framed.contains(&format!("addressed to: {}", addressee.as_str())),
                "{addressee:?} did not name itself: {framed}"
            );
            assert!(
                framed.contains("the migration has to be reversible"),
                "{addressee:?} did not carry the text: {framed}"
            );
        }
        // The supervisor is told outright that the update is not an instruction
        // to it — the failure this replaces is a judge acting on the worker's
        // amendment rather than judging against it.
        assert!(note(Addressee::Supervisor)
            .framed()
            .contains("not an instruction to you"));
        // And a note that *binds* says so, because what it binds is what the
        // work is judged against.
        assert!(note(Addressee::Worker)
            .binding()
            .framed()
            .contains("judged"));
        assert!(!note(Addressee::Worker).framed().contains("judged"));

        // A note with nothing in it is refused where it is made: delivering one
        // spends a turn on a redirection that says nothing.
        for blank in ["", "   \n\t"] {
            assert!(Note::new(Addressee::Worker, blank).is_err(), "{blank:?}");
        }
    }

    /// The live side is the conversation's own, and it is read back exactly as
    /// the engine reported it.
    #[test]
    fn the_live_side_is_what_the_conversation_reported() {
        let live = LiveTurn::new();
        assert!(
            !live.agent_is_live(),
            "a member with no turn open read as working"
        );

        live.opened(Party::Assistant);
        assert!(
            live.agent_is_live(),
            "the worker's turn did not read as the worker's"
        );

        live.opened(Party::User);
        assert!(
            !live.agent_is_live(),
            "the judge's turn read as the worker's"
        );

        live.closed();
        assert!(
            !live.agent_is_live(),
            "a turn that closed left the worker reading as live"
        );
    }

    /// A note offered while no worker turn is open is queued for the next one,
    /// and one offered while a worker turn is open is delivered into it.
    ///
    /// All three states of the conversation are driven, because the routing is a
    /// decision about which one it is in: **between turns**, with neither party
    /// taking one; the **judge** deciding; and the **worker** working. The first
    /// two queue and the third delivers, and the delivery is asserted by where it
    /// failed — against a binary that is not there, which is a delivery really
    /// attempted rather than a note quietly held.
    ///
    /// `tests/e2e/note.rs` drives the two live ones against a real conversation
    /// and a real control socket; this is where the between-turns state is
    /// reachable at all, because a run reaches it only inside a turn boundary
    /// nothing outside the member can observe.
    #[test]
    fn a_note_is_routed_by_which_side_of_the_conversation_is_live() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::bind(dir.path()).expect("a spool");
        let live = std::sync::Arc::new(LiveTurn::new());
        let mut router = router(spool.clone(), &live);
        let emitter = emitter("worker");

        // Neither party is taking a turn: queued for the next one.
        spool.offer("a", &note(Addressee::Worker)).expect("offered");
        router.service(&emitter);
        assert_eq!(
            spool.answered("a"),
            Some(NoteDelivery::Accepted(Accepted::Queued)),
            "a note offered between turns was not queued for the next one"
        );

        // The judge is deciding: queued too, for the turn its response opens.
        live.opened(Party::User);
        spool.offer("b", &note(Addressee::Worker)).expect("offered");
        router.service(&emitter);
        assert_eq!(
            spool.answered("b"),
            Some(NoteDelivery::Accepted(Accepted::Queued)),
            "a note offered while the judge was deciding was not queued"
        );

        // The worker's turn opens, and both held notes go into it.
        live.opened(Party::Assistant);
        router.service(&emitter);
        assert!(router.held.is_empty(), "a queued note was never delivered");

        // And one offered while the worker's turn is already open is delivered
        // straight away rather than queued.
        spool.offer("c", &note(Addressee::Worker)).expect("offered");
        router.service(&emitter);
        assert!(
            matches!(
                spool.answered("c"),
                Some(NoteDelivery::Undelivered(Undelivered::NoConversation { reason }))
                    if reason.contains("oneagentgraph-no-such-oneharness")
            ),
            "a note offered into a live worker turn was not delivered into it: {:?}",
            spool.answered("c")
        );
    }

    /// A note whose only addressee is the supervisor is refused, rather than
    /// handed to the worker under a frame naming somebody else.
    ///
    /// The supervisor's own copy reaches it through the effective task and the
    /// completion criteria the conversation layer composes, and the onejudge this
    /// build links exposes no way to add to either mid-run. Delivering it to the
    /// worker instead would be the note's addressee never seeing it and the wrong
    /// party acting on it — so the caller is told, which is what lets it choose.
    #[test]
    fn a_note_only_the_supervisor_is_addressed_by_is_refused_rather_than_misrouted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::bind(dir.path()).expect("a spool");
        let live = std::sync::Arc::new(LiveTurn::new());
        live.opened(Party::Assistant);
        let mut router = router(spool.clone(), &live);
        router.service(&emitter("worker"));

        spool
            .offer("a", &note(Addressee::Supervisor))
            .expect("offered");
        router.service(&emitter("worker"));
        assert!(
            matches!(
                spool.answered("a"),
                Some(NoteDelivery::Undelivered(Undelivered::NoConversation { reason }))
                    if reason.contains("supervisor")
            ),
            "{:?}",
            spool.answered("a")
        );

        // A note the worker is *also* addressed by still reaches the worker: an
        // amendment binds both parties, and refusing it because one half has no
        // path would deliver nothing at all.
        spool.offer("b", &note(Addressee::Both)).expect("offered");
        router.service(&emitter("worker"));
        assert!(
            matches!(
                spool.answered("b"),
                Some(NoteDelivery::Undelivered(Undelivered::NoConversation { reason }))
                    if reason.contains("oneagentgraph-no-such-oneharness")
            ),
            "{:?}",
            spool.answered("b")
        );
    }

    /// A conversation that is over refuses a note instead of taking one nothing
    /// will read — and a note already in the spool when it ends is answered by
    /// the same fact rather than left for its caller to time out on.
    #[test]
    fn a_completed_conversation_refuses_a_note_rather_than_accepting_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = Spool::bind(dir.path()).expect("a spool");
        let live = std::sync::Arc::new(LiveTurn::new());
        let mut router = router(spool.clone(), &live);
        let emitter = emitter("worker");

        // In the spool when the conversation ends, and answered by it.
        spool.offer("a", &note(Addressee::Worker)).expect("offered");
        router.complete("its supervisor judged the task complete", &emitter);
        assert!(
            matches!(
                spool.answered("a"),
                Some(NoteDelivery::Undelivered(Undelivered::ConversationCompleted {
                    completion_reason
                })) if completion_reason.contains("judged the task complete")
            ),
            "{:?}",
            spool.answered("a")
        );

        // And offered afterwards: refused before it is spooled at all, because a
        // conversation that is over will never service its spool again.
        let refused = submit(dir.path(), &note(Addressee::Worker))
            .expect_err("a completed conversation cannot take a note");
        assert!(
            matches!(&refused, Undelivered::ConversationCompleted { completion_reason }
                if completion_reason.contains("judged the task complete")),
            "{refused:?}"
        );
        assert!(
            refused.to_string().contains("was not delivered"),
            "{refused}"
        );
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
        std::fs::write(spool.request("998-1"), "not a note").expect("write");
        assert!(
            spool.take().is_empty(),
            "a note this build cannot read was acted on"
        );
    }
}
