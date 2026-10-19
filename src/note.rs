//! Role-addressed notes: an update to one party's task, carried from outside a
//! run into the two-party conversation that is having it.
//!
//! `interrupt` redirects the turn an operator addresses, and that is the whole of
//! what it can do. A note is routed instead: it is offered to the member itself,
//! the one thing that knows which side of its conversation is live, and it
//! reaches either party — the worker's turn or the judge's.
//!
//! # The shapes are onejudge's, not this crate's
//!
//! [`Addressee`], [`Delivery`], [`Note`], [`Accepted`] and [`Undelivered`] are
//! **re-exports** of [`onejudge::note`], which declares them — the note contract
//! of the two-party conversation that receives them, the message
//! `agent.note@1`. Nothing about them is declared here, and that is deliberate —
//! a second declaration is a shape that drifts, and a note that satisfies the
//! copy is still refused by the conversation it was written for.
//!
//! # The routing is onejudge's
//!
//! Which side of a member is live is a fact only the engine driving it has, so
//! this crate does not decide it. The engine's own end of the channel
//! ([`onejudge::note::NoteInbox`]) goes onto the [`onejudge::cli::Plan`]
//! [`crate::judge`] drives, and what it does with a note is the contract's:
//!
//! * **A live turn of the party it is addressed to, opened with control** — the
//!   turn is aborted and redirected, and its replacement opens with the note:
//!   [`Accepted::Interrupted`], naming that party. Only a [`Delivery::Interrupt`]
//!   note (the default) does this.
//! * **Any other live turn** — a [`Delivery::TurnEnd`] note, a turn opened
//!   without control, a note for the other party, a redirect oneharness refused —
//!   the note waits for that turn to end and reaches the turn after it:
//!   [`Accepted::AtTurnEnd`].
//! * **A supervisor decision that completes with the note in hand** —
//!   [`Accepted::JudgedWith`]: the work was passed with it, and there was no next
//!   worker turn to deliver it into.
//! * **Between turns** — the next turn to open takes it: [`Accepted::Queued`].
//! * **The conversation is over** — [`Undelivered`], naming which. Never a silent
//!   acceptance.
//!
//! The engine takes a note out of its inbox only when it is about to present it,
//! so until then the note is the caller's to take back — which is what
//! [`NoteOffer::withdraw`] and [`NoteOffer::wait_until`] rest on.
//!
//! # The transport: an offer, carried in by the member's relay
//!
//! A note is offered by a *different process* from the one running the member —
//! `oneagentgraph`'s own API, against a run's state directory. The two meet at the
//! member's notes spool (`Endpoint`), the bus inbox's [`Spool`] backend, which
//! carries one `Request` at a time: offer this note, wait for that offer's answer
//! (with or without a deadline), take that offer back.
//!
//! The member's `Relay` — one thread of the member's process — services them. It
//! offers each note into the conversation's inbox **in process**, with the bus's
//! own [`onemessagebus::Sender::offer`], and answers the caller that the note is
//! admitted only once it is: the note is then in the onejudge inbox, untaken. It
//! holds that in-process [`onemessagebus::Offer`] for the caller, so the wait, the
//! deadline and the withdrawal the caller asks for are the bus's own — taking a
//! note and taking it back never both succeed — rather than anything this crate
//! re-implements.
//!
//! It is a relay rather than the spool bound straight onto the engine's inbox for
//! the things only the member process can do with an offer: give it an id the
//! run's stream names it by, publish what became of it there (`note-admitted`,
//! `turn-interrupted`, `note-withdrawn`), count it as offered so a paced
//! conversation holding between turns opens the turn that will take it, and keep
//! the record `Deliveries` attributes a turn from.
//!
//! Every answer is also written down in the member's scratch ([`OFFERS_DIR`])
//! before it is given, so a caller that asks after the member has settled — when
//! nothing services the spool any more — still reads what became of its note
//! rather than a refusal that would be untrue of a note already delivered.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use onemessagebus::{Closed, Delivered, Disposition, Inbox, Message, SchemaId, Unanswered};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::{
    Emitter, EventKind, NoteAccepted, NoteAdmitted, NoteWithdrawn, TurnInterrupted, WithdrawnReason,
};
use crate::member::as_payload;

pub use onejudge::note::{
    Accepted, Addressee, Criterion, DeliveredNote, Delivery, Note, NoteRefused, NoteText, Party,
    Undelivered,
};
pub use onemessagebus::Spool;

/// The directory a two-party member binds inside its own scratch to receive
/// notes for the conversation's life — a sibling of
/// [`crate::control::CONTROL_FILE`].
pub const NOTES_DIR: &str = "notes";

/// The directory beside [`NOTES_DIR`] where a member writes down what became of
/// each offer, one file per offer, before it answers the caller.
pub const OFFERS_DIR: &str = "note-offers";

/// The shape every document in a member's spool is written under: the bus
/// spool's own, refused by number when a build that knew more wrote it.
pub const NOTE_SCHEMA_VERSION: u32 = onemessagebus::SPOOL_SCHEMA_VERSION;

/// The shape of one offer's written-down answer in [`OFFERS_DIR`]. A file naming
/// another version is not read, and the caller is answered from the spool alone.
const SETTLED_SCHEMA_VERSION: u32 = 1;

/// How often the [`Relay`] looks for a request when none is waiting, which is
/// also how soon it notices it is asked to stop.
const RELAY_POLL: Duration = Duration::from_millis(50);

/// How long a member's end waits for its relay to stop taking requests.
const STOP_BOUND: Duration = Duration::from_secs(2);

/// How long a member's end waits for the offers it holds to be answered before it
/// closes its spool. Every wait returns at once on the ordinary path — the
/// conversation's inbox is closed by then, which answers every note in it — so
/// this bound is met only by a member condemned with a redirect in flight.
const SETTLE_BOUND: Duration = Duration::from_secs(5);

/// How long a worker turn being announced waits for a note already handed to the
/// conversation to be answered, before it is announced without one.
///
/// The engine answers a note it takes *before* it announces the turn that
/// carries it, and the turn being announced looks for that answer itself
/// ([`Book::probe`]) — or, for a note a caller's wait holds, lets that wait's
/// thread report it a moment later — so the wait is that moment, and this bound
/// is only ever reached by a note that arrived after the engine last looked,
/// which the announced turn does not carry.
const DELIVERY_SETTLE: Duration = Duration::from_secs(1);

/// The longest offer id a member accepts — the id names a file.
const MAX_OFFER_ID: usize = 128;

/// What became of one note.
///
/// The three are not interchangeable and none is an exit code: a caller reads
/// the [`Accepted`] to know *when* the addressee sees it, the [`Undelivered`] to
/// know it never will, and [`NoteDelivery::Expired`] to know its own deadline
/// passed with the note untaken — and that no turn ever will take it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteDelivery {
    /// The conversation took it.
    Accepted(Accepted),
    /// It was not delivered.
    Undelivered(Undelivered),
    /// The deadline [`NoteOffer::wait_until`] was given passed with the note
    /// still untaken, so it was withdrawn: it was never presented, and never
    /// will be.
    Expired,
}

/// What offering one note answered: it is in the member's conversation inbox,
/// or it never got there.
#[derive(Debug)]
pub enum NoteOffered {
    /// The note sits in the member's onejudge inbox, untaken; the offer is how
    /// its sender waits for it or takes it back.
    Admitted(NoteOffer),
    /// It was not admitted, and this is why.
    Undelivered(Undelivered),
}

/// One admitted note: what waits for its answer, or takes it back.
///
/// Taking it and withdrawing it never both succeed and never both fail — that is
/// the bus's own guarantee for the in-process offer the member holds, carried to
/// this process over the member's spool. Dropping one withdraws nothing: the
/// note stays offered, and what became of it is still published on the run's
/// stream.
#[derive(Debug)]
pub struct NoteOffer {
    id: OfferId,
    state: OfferState,
}

#[derive(Debug)]
enum OfferState {
    /// Held by the member's relay, reached through the spool in this scratch.
    Relayed { scratch: PathBuf },
    /// Answered already, by a member that has no offer to hold: a run started by
    /// a build whose endpoint took a bare note, or a single-sided member whose one
    /// lever answered at once. There is nothing to take back.
    Answered(NoteDelivery),
}

impl NoteOffer {
    /// An offer whose answer is already known, with nothing to take back.
    pub(crate) fn answered(delivery: NoteDelivery) -> Self {
        Self {
            id: OfferId::mint(),
            state: OfferState::Answered(delivery),
        }
    }

    /// This offer's id: unique per offer within the run, and the id the run's
    /// `note-admitted`, `turn-interrupted` and `note-withdrawn` events name it by.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id.0
    }

    /// Block until the conversation has answered the note.
    ///
    /// # Errors
    ///
    /// None today; the `Result` is the room a later reason to fail the wait
    /// itself — rather than to answer about the note — needs. Every answer about
    /// the note, its not having been delivered included, is a [`NoteDelivery`].
    pub fn wait(self) -> Result<NoteDelivery, crate::error::Error> {
        Ok(match self.state {
            OfferState::Answered(delivery) => delivery,
            OfferState::Relayed { scratch } => {
                let asked = ask(
                    &scratch,
                    Request::Wait {
                        offer: self.id.clone(),
                    },
                );
                resolve(&scratch, &self.id, asked)
            }
        })
    }

    /// [`wait`](Self::wait), withdrawing the note if it is still untaken at
    /// `deadline`: then the answer is [`NoteDelivery::Expired`] and no turn ever
    /// takes it. A note taken before the deadline is waited on past it, and its
    /// answer returned however late it comes.
    ///
    /// # Errors
    ///
    /// As [`wait`](Self::wait).
    pub fn wait_until(self, deadline: Instant) -> Result<NoteDelivery, crate::error::Error> {
        Ok(match self.state {
            OfferState::Answered(delivery) => delivery,
            OfferState::Relayed { scratch } => {
                // Named as an instant on the clock both processes read, never as
                // a span: a span would restart when the member received it, and
                // the time the request spent crossing the spool would extend the
                // deadline the caller set.
                // A deadline past what the wall clock can represent is one no
                // caller lives to see, so it is asked as the latest instant the
                // wire can name rather than allowed to overflow the clock.
                let left = deadline.saturating_duration_since(Instant::now());
                let asked = ask(
                    &scratch,
                    Request::WaitUntil {
                        offer: self.id.clone(),
                        deadline_unix_ms: SystemTime::now()
                            .checked_add(left)
                            .map_or(u64::MAX, unix_millis),
                    },
                );
                resolve(&scratch, &self.id, asked)
            }
        })
    }

    /// Take the note back: `Ok` only when no turn had taken it, and then none
    /// ever will.
    ///
    /// # Errors
    ///
    /// This offer, still able to [`wait`](Self::wait) for its answer, when a turn
    /// took the note first — or when that cannot be established, which reads as
    /// taken.
    pub fn withdraw(self) -> Result<(), Self> {
        let OfferState::Relayed { scratch } = &self.state else {
            return Err(self);
        };
        match ask(
            scratch,
            Request::Withdraw {
                offer: self.id.clone(),
            },
        ) {
            Ok(Reply::Withdrawn) => Ok(()),
            _ => Err(self),
        }
    }
}

/// `at`, in milliseconds since the Unix epoch: how a deadline crosses from the
/// caller's process to the member's.
///
/// The wall clock, because it is the one clock two processes on one host can
/// name the same instant on without a handshake — an `Instant` means nothing
/// outside the process that took it. A step of that clock between the two reads
/// moves the deadline by the step; that is the price of naming an instant at all,
/// and it is bounded by the clock rather than by how long the request took.
fn unix_millis(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

/// One offer's id, as a caller mints it and a member names its events and its
/// written-down answer by.
///
/// Checked as it arrives, because it crosses a trust boundary — the member reads
/// it out of a spool any process can write into — and names a file in the
/// member's scratch: letters, digits and `-`, starting with a letter or digit, at
/// most [`MAX_OFFER_ID`] long.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct OfferId(String);

impl OfferId {
    /// A fresh id: the instant, this process, and a count within it, so two
    /// offers from one run — or from two processes offering into it — never share
    /// one.
    fn mint() -> Self {
        static MINTED: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        Self(format!(
            "note-{nanos}-{}-{}",
            std::process::id(),
            MINTED.fetch_add(1, Ordering::SeqCst)
        ))
    }
}

impl TryFrom<String> for OfferId {
    type Error = String;

    fn try_from(id: String) -> Result<Self, String> {
        let shaped = !id.is_empty()
            && id.len() <= MAX_OFFER_ID
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !id.starts_with('-');
        if shaped {
            Ok(Self(id))
        } else {
            Err(format!(
                "{id:?} is not an offer id: 1 to {MAX_OFFER_ID} ASCII letters, digits and `-`, \
                 not starting with `-`"
            ))
        }
    }
}

impl From<OfferId> for String {
    fn from(id: OfferId) -> Self {
        id.0
    }
}

impl std::fmt::Display for OfferId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a caller asks of a member's note spool: the message
/// `oneagentgraph.note-request@1`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    /// Admit `note` to the conversation's inbox, as offer `offer`.
    Offer {
        /// The id the offer is known by from here on.
        offer: OfferId,
        /// The note.
        note: Note,
    },
    /// Answer once the conversation has.
    Wait {
        /// Which offer.
        offer: OfferId,
    },
    /// As [`Request::Wait`], withdrawing the note if it is still untaken at
    /// `deadline_unix_ms`.
    WaitUntil {
        /// Which offer.
        offer: OfferId,
        /// The deadline, in milliseconds since the Unix epoch — see
        /// [`unix_millis`] for why that clock.
        deadline_unix_ms: u64,
    },
    /// Take the note back, if no turn has taken it.
    Withdraw {
        /// Which offer.
        offer: OfferId,
    },
}

impl Message for Request {
    const SCHEMA: SchemaId = SchemaId::literal("oneagentgraph", "note-request", 1);
}

/// What a member answers one [`Request`] with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Reply {
    /// The note is in the conversation's inbox, untaken.
    Admitted,
    /// The conversation took it.
    Accepted(Accepted),
    /// It was not delivered.
    Undelivered(Undelivered),
    /// The deadline passed with it untaken; it was withdrawn.
    Expired,
    /// It was taken back before any turn took it.
    Withdrawn,
    /// A turn took it first, or its answer is already being waited on: there is
    /// nothing to take back.
    Taken,
    /// The member holds no such offer.
    Refused {
        /// Why.
        reason: String,
    },
}

impl Disposition for Reply {}

/// One offer's answer, as the member writes it down in [`OFFERS_DIR`].
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settled {
    schema_version: u32,
    reply: Reply,
}

/// Offer `note` to the conversation running in `scratch`, and answer whether it
/// is in.
///
/// Which protocol the member's endpoint speaks is read off the schema its spool
/// declares rather than assumed: a run started by a build whose endpoint took a
/// bare note is still delivered to — answered when it is, with nothing to take
/// back.
pub(crate) fn offer(scratch: &Path, note: &Note) -> NoteOffered {
    let address = scratch.join(NOTES_DIR);
    if !address.is_dir() {
        return NoteOffered::Undelivered(no_endpoint());
    }
    // llmlint: ignore-block[changed_behavior_has_e2e] an endpoint that takes a
    // bare note is bound only by a build before the offer protocol, so the run
    // this branch reaches is one an earlier *release* started — a journey would
    // have to run that release inside the suite. What it does is
    // `an_endpoint_that_takes_a_bare_note_is_still_delivered_to` below, driven
    // across a real spool into a real conversation inbox.
    if Spool::declared(&address).ok().flatten() == Some(Note::SCHEMA) {
        return match submit(scratch, note) {
            Ok(accepted) => {
                NoteOffered::Admitted(NoteOffer::answered(NoteDelivery::Accepted(accepted)))
            }
            Err(undelivered) => NoteOffered::Undelivered(undelivered),
        };
    }
    // llmlint: ignore-end[changed_behavior_has_e2e]
    let offer = OfferId::mint();
    match ask(
        scratch,
        Request::Offer {
            offer: offer.clone(),
            note: note.clone(),
        },
    ) {
        Ok(Reply::Admitted) => NoteOffered::Admitted(NoteOffer {
            id: offer,
            state: OfferState::Relayed {
                scratch: scratch.to_path_buf(),
            },
        }),
        Ok(Reply::Undelivered(undelivered)) | Err(undelivered) => {
            NoteOffered::Undelivered(undelivered)
        }
        Ok(other) => NoteOffered::Undelivered(Undelivered::NoConversation {
            reason: format!(
                "the member answered the offer with {other:?} rather than admitting it"
            ),
        }),
    }
}

/// Hand `note` to the conversation running in `scratch`, and answer what became
/// of it: an offer, waited on.
///
/// # Errors
///
/// [`Undelivered`], for a note the conversation did not take: it had already
/// completed, it had ended, or nothing ever took it.
pub fn submit(scratch: &Path, note: &Note) -> Result<Accepted, Undelivered> {
    submit_within(scratch, note, onemessagebus::SPOOL_WAIT)
}

/// [`submit`], with the wait made a parameter so a test can drive the deadline
/// itself rather than sitting out the real one.
fn submit_within(scratch: &Path, note: &Note, wait: Duration) -> Result<Accepted, Undelivered> {
    let address = scratch.join(NOTES_DIR);
    // Said in this crate's words rather than the backend's, because the caller
    // asked about a member rather than a directory.
    if !address.is_dir() {
        return Err(no_endpoint());
    }
    if Spool::declared(&address).ok().flatten() == Some(Request::SCHEMA) {
        return match offer(scratch, note) {
            NoteOffered::Admitted(offer) => match offer.wait() {
                Ok(NoteDelivery::Accepted(accepted)) => Ok(accepted),
                Ok(NoteDelivery::Undelivered(undelivered)) => Err(undelivered),
                // An offer waited on without a deadline never expires; a reading
                // that says otherwise is a member answering out of turn.
                Ok(NoteDelivery::Expired) | Err(_) => Err(Undelivered::NoConversation {
                    reason: "the member answered a wait with no deadline as expired".to_string(),
                }),
            },
            NoteOffered::Undelivered(undelivered) => Err(undelivered),
        };
    }
    Spool::connect_within::<Note, Accepted>(address, wait)
        .send(note.clone())
        .map_err(Undelivered::from)
}

/// The refusal for a member that binds no note endpoint.
fn no_endpoint() -> Undelivered {
    Undelivered::NoConversation {
        reason: "this member binds no note endpoint: only a two-party member's own thread does, \
                 and this run recorded none for it"
            .to_string(),
    }
}

/// Send one request into the member's spool and read its answer.
fn ask(scratch: &Path, request: Request) -> Result<Reply, Undelivered> {
    Spool::connect::<Request, Reply>(scratch.join(NOTES_DIR))
        .send(request)
        .map_err(Undelivered::from)
}

/// What one wait answered, read back as a [`NoteDelivery`].
///
/// The member's own answer first; and where there is none — the member settled,
/// and its spool takes no more — what it wrote down before it went, which is the
/// truth about a note it had already answered.
fn resolve(scratch: &Path, offer: &OfferId, asked: Result<Reply, Undelivered>) -> NoteDelivery {
    if let Ok(reply) = &asked {
        if let Some(delivery) = delivery_of(reply) {
            return delivery;
        }
    }
    if let Some(delivery) = read_settled(scratch, offer).as_ref().and_then(delivery_of) {
        return delivery;
    }
    NoteDelivery::Undelivered(match asked {
        Err(undelivered) => undelivered,
        Ok(Reply::Refused { reason }) => Undelivered::NoConversation { reason },
        Ok(other) => Undelivered::NoConversation {
            reason: format!("the member answered a wait with {other:?}"),
        },
    })
}

/// The delivery a reply answers, when it answers one.
fn delivery_of(reply: &Reply) -> Option<NoteDelivery> {
    match reply {
        Reply::Accepted(accepted) => Some(NoteDelivery::Accepted(accepted.clone())),
        Reply::Undelivered(undelivered) => Some(NoteDelivery::Undelivered(undelivered.clone())),
        Reply::Expired => Some(NoteDelivery::Expired),
        Reply::Withdrawn => Some(NoteDelivery::Undelivered(Undelivered::NoConversation {
            reason: "the note was withdrawn by its sender before any turn took it".to_string(),
        })),
        Reply::Admitted | Reply::Taken | Reply::Refused { .. } => None,
    }
}

/// Where one offer's answer is written down.
fn settled_path(scratch: &Path, offer: &OfferId) -> PathBuf {
    scratch.join(OFFERS_DIR).join(format!("{offer}.json"))
}

/// The answer a member wrote down for `offer`, if it wrote one this build reads.
fn read_settled(scratch: &Path, offer: &OfferId) -> Option<Reply> {
    let raw = std::fs::read_to_string(settled_path(scratch, offer)).ok()?;
    let settled: Settled = serde_json::from_str(&raw).ok()?;
    (settled.schema_version == SETTLED_SCHEMA_VERSION).then_some(settled.reply)
}

/// Write `offer`'s answer down, beside its final name and renamed onto it, so a
/// caller reading it never reads half of one.
///
/// Best-effort: an answer that could not be written is still given, and only a
/// caller that asks after the member has gone loses it — that caller is then
/// answered from the spool's own close, which names how the member ended.
// llmlint: ignore-block[changed_behavior_has_e2e] the failing arms are the host
// refusing a write inside the member's own scratch, which this run created and
// owns: no graph, task or config reaches them. What they decide — the live
// caller is still answered, and a late one falls back to the close — is driven by
// `an_unwritable_answer_store_still_answers_the_live_caller` below, which
// occupies the store's path so the write really fails.
fn write_settled(scratch: &Path, offer: &OfferId, reply: &Reply) {
    let dir = scratch.join(OFFERS_DIR);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(rendered) = serde_json::to_string(&Settled {
        schema_version: SETTLED_SCHEMA_VERSION,
        reply: reply.clone(),
    }) else {
        return;
    };
    let partial = dir.join(format!("{offer}.json.partial"));
    if std::fs::write(&partial, rendered).is_ok() {
        let _ = std::fs::rename(&partial, settled_path(scratch, offer));
    }
}
// llmlint: ignore-end[changed_behavior_has_e2e]

/// What a member with **no conversation** is handed instead: the addressed role,
/// then the note's own text, unchanged.
///
/// A single-sided `kind: oneharness` member has one party and one lever, so a
/// note to it falls through to [`crate::control::interrupt`] — see
/// [`crate::control::offer_note`]. There is no conversation layer to frame it, so
/// the frame is written here, and it says the one thing the two-party framing
/// exists to say: whose task this update belongs to.
#[must_use]
pub(crate) fn framed(note: &Note) -> String {
    format!(
        "— run note (addressed to: {}) —\n{}\n\n{}",
        note.addressee.as_str(),
        "The following update was delivered to this member's task; act on it.",
        note.text
    )
}

/// The inbox a member's relay takes requests from.
type Requests = Inbox<Request, Reply>;

/// Where one member receives notes: its own request inbox, bound over a spool in
/// its scratch for the conversation's life.
///
/// A directory rather than a socket, and that is the one deviation from the
/// approved contract's wording: the record still names the endpoint by path, and
/// the two ends still meet nowhere else, but a member of this crate runs on
/// Windows too — where a unix domain socket is exactly why [`crate::control`]
/// already reports *no controllable turn* — and a note seam that existed on one
/// platform only would be a delivery an operator could not rely on.
pub(crate) struct Endpoint {
    requests: Arc<Requests>,
    spool: Spool,
    relayed: Arc<Relayed>,
    scratch: PathBuf,
}

impl Endpoint {
    /// Bind this member's endpoint, the way its thread does before its plan is
    /// built.
    ///
    /// [`None`] when the spool cannot be bound, which is a member with no note
    /// endpoint — reported to a caller as one rather than failing a run that is
    /// otherwise fine, exactly as a control record that could not be written is.
    #[must_use]
    pub(crate) fn bind(scratch: &Path) -> Option<Self> {
        let requests = Requests::new();
        let spool = Spool::bind(scratch.join(NOTES_DIR), &requests).ok()?;
        Some(Self {
            requests: Arc::new(requests),
            spool,
            relayed: Arc::default(),
            scratch: scratch.to_path_buf(),
        })
    }

    /// Where it is — the spool's address as the backend states it, which is what
    /// a member's control record names.
    #[must_use]
    pub(crate) fn address(&self) -> &Path {
        self.spool.address()
    }

    /// The record the relay keeps of the notes it handed over, for
    /// [`Deliveries`].
    #[must_use]
    pub(crate) fn relayed(&self) -> Arc<Relayed> {
        Arc::clone(&self.relayed)
    }
}

/// What the [`Relay`] has handed to the conversation: how many notes are with it
/// unanswered, and how many it took.
#[derive(Default)]
pub(crate) struct Relayed {
    state: Mutex<Handed>,
    answered: Condvar,
    /// The offers behind the count, once the relay is open: what a turn being
    /// announced asks whether the engine has answered, so a note is counted
    /// when the engine answers it rather than when its caller next waits.
    book: OnceLock<Weak<Book>>,
}

#[derive(Debug, Default)]
struct Handed {
    /// Notes handed to the conversation and not yet answered.
    in_flight: usize,
    /// Notes the conversation accepted into a worker turn it has yet to
    /// announce — see [`opens_a_later_turn`].
    taken: usize,
}

impl Relayed {
    fn handing(&self) {
        self.lock().in_flight += 1;
    }

    fn answered(&self, accepted: bool) {
        let mut state = self.lock();
        state.in_flight = state.in_flight.saturating_sub(1);
        if accepted {
            state.taken += 1;
        }
        drop(state);
        self.answered.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, Handed> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many notes the conversation has taken, once every note it was handed
    /// has been answered — or once [`DELIVERY_SETTLE`] says the one still out is
    /// not an answer this moment is waiting on.
    ///
    /// Each look first settles every held offer the engine has already answered
    /// ([`Book::probe`]), so a note nobody is waiting on is counted for the turn
    /// that carries it, not for whichever turn is announced after its caller
    /// finally waits.
    fn taken(&self, within: Duration) -> usize {
        let deadline = Instant::now() + within;
        loop {
            if let Some(book) = self.book.get().and_then(Weak::upgrade) {
                book.probe();
            }
            let state = self.lock();
            let left = deadline.saturating_duration_since(Instant::now());
            if state.in_flight == 0 || left.is_zero() {
                return state.taken;
            }
            drop(
                self.answered
                    .wait_timeout(state, left.min(RELAY_POLL))
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
    }
}

/// The conversation's own record of what it has taken, read at each worker turn
/// this graph announces — what lets [`crate::judge`] stamp
/// [`crate::event::Origin::Delivered`] on exactly the turn that received a note.
///
/// Authoritative rather than inferred. The engine answers a note it takes
/// *before* it pushes the note onto the transcript and announces the turn that
/// opens on it — as the first turn's opening message, as the next turn's, or in
/// front of the supervisor's own words — and the relay counts that answer. A turn
/// being announced waits for any note already handed over to be answered, so the
/// count names every note that turn carries and none it does not. A note the
/// conversation refused is never counted. Nothing about the *text* is consulted:
/// a note's words can legitimately occur in the task or in a supervisor's prose,
/// and reading their presence as a delivery would stamp an unrelated turn as the
/// operator's.
///
/// One per member, owned by the sink that announces its turns; a member with no
/// note seam holds no record and attributes nothing.
pub(crate) struct Deliveries {
    relayed: Option<Arc<Relayed>>,
    /// How many of the record's notes a turn has already been stamped for.
    attributed: usize,
}

impl Deliveries {
    /// The record behind a member's [`Endpoint`]. `None` is a member with no note
    /// seam, for which nothing is ever delivered.
    pub(crate) fn of(relayed: Option<Arc<Relayed>>) -> Self {
        Self {
            relayed,
            attributed: 0,
        }
    }

    /// Whether the worker turn being announced now carries a note taken since
    /// the last one this was asked about.
    ///
    /// Asked once per worker turn, in the order the engine announces them, which
    /// is what makes the answer exact: every note the record has gained since
    /// the last turn rides this one, and is counted as attributed so the next
    /// turn is judged on its own deliveries alone.
    pub(crate) fn carried_by_this_turn(&mut self) -> bool {
        let Some(relayed) = &self.relayed else {
            return false;
        };
        let taken = relayed.taken(DELIVERY_SETTLE);
        let fresh = taken > self.attributed;
        self.attributed = taken;
        fresh
    }
}

/// One admitted note the relay holds for its caller.
struct Held {
    note: Note,
    /// The in-process offer, until a wait takes it away to block on — `None`
    /// while one does, which is also what refuses a second wait or a withdrawal
    /// then.
    offer: Option<onemessagebus::Offer<Accepted>>,
}

/// Every offer a member holds, and what it does as each is answered: write the
/// answer down, publish it, and count it.
struct Book {
    scratch: PathBuf,
    emitter: Emitter,
    relayed: Arc<Relayed>,
    held: Mutex<HashMap<OfferId, Held>>,
    /// Notified whenever an offer is answered, for an [`Ending`] waiting on them.
    emptied: Condvar,
}

impl Book {
    fn lock(&self) -> MutexGuard<'_, HashMap<OfferId, Held>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `offer` was ever made to this member.
    fn knows(&self, offer: &OfferId) -> bool {
        self.lock().contains_key(offer) || settled_path(&self.scratch, offer).exists()
    }

    /// Take `offer`'s in-process handle out to act on, with its note.
    fn take(&self, offer: &OfferId) -> Option<(onemessagebus::Offer<Accepted>, Note)> {
        let mut held = self.lock();
        let entry = held.get_mut(offer)?;
        let handle = entry.offer.take()?;
        Some((handle, entry.note.clone()))
    }

    /// Put a handle a withdrawal could not take back where it was.
    fn restore(&self, offer: &OfferId, handle: onemessagebus::Offer<Accepted>) {
        if let Some(entry) = self.lock().get_mut(offer) {
            entry.offer = Some(handle);
        }
    }

    /// Every offer no wait is blocked on, taken out to settle.
    fn idle(&self) -> Vec<(OfferId, onemessagebus::Offer<Accepted>, Note)> {
        self.lock()
            .iter_mut()
            .filter_map(|(id, entry)| {
                let handle = entry.offer.take()?;
                Some((id.clone(), handle, entry.note.clone()))
            })
            .collect()
    }

    /// `offer` is answered `reply`: written down first, so a caller that reads it
    /// after the member has gone reads this; then published, counted, and let go.
    fn settle(&self, offer: &OfferId, note: &Note, reply: &Reply) {
        write_settled(&self.scratch, offer, reply);
        self.lock().remove(offer);
        self.announce(offer, note, reply);
    }

    /// Publish and count an answer already written down and let go of.
    fn announce(&self, offer: &OfferId, note: &Note, reply: &Reply) {
        self.publish(offer, note, reply);
        self.relayed.answered(match reply {
            Reply::Accepted(accepted) => opens_a_later_turn(accepted),
            _ => false,
        });
        self.emptied.notify_all();
    }

    /// Settle every held offer the engine has already answered, without waiting
    /// and without taking any offer out of the book.
    ///
    /// Looked in place under the book's lock, through the bus's non-consuming
    /// `try_answer`: an offer still untaken keeps every guarantee it had, so a
    /// withdrawal or a deadline racing this still settles it atomically. One
    /// that answered is written down and dropped from the book before the lock
    /// is let go, so a wait or a withdrawal arriving next reads the written
    /// answer rather than settling it a second time. One a wait has taken out is
    /// that wait's to settle, and is left to it.
    fn probe(&self) {
        let answered: Vec<(OfferId, Note, Reply)> = {
            let mut held = self.lock();
            let answered: Vec<(OfferId, Note, Reply)> = held
                .iter_mut()
                .filter_map(|(id, entry)| {
                    let reply = match entry.offer.as_mut()?.try_answer()? {
                        Ok(accepted) => Reply::Accepted(accepted),
                        Err(undelivered) => Reply::Undelivered(Undelivered::from(undelivered)),
                    };
                    Some((id.clone(), entry.note.clone(), reply))
                })
                .collect();
            for (offer, _, reply) in &answered {
                write_settled(&self.scratch, offer, reply);
                held.remove(offer);
            }
            answered
        };
        for (offer, note, reply) in &answered {
            self.announce(offer, note, reply);
        }
    }

    /// Publish what became of `offer` on the run's own stream.
    ///
    /// A caller learns the disposition from its own wait; this is the *run's*
    /// record of it, so an operator reading the journal sees a note arrive and
    /// land without having sent it.
    fn publish(&self, offer: &OfferId, note: &Note, reply: &Reply) {
        let member = self.emitter.member().unwrap_or_default().to_string();
        let interrupted = |delivered: bool, reason: Option<String>, accepted: Option<&Accepted>| {
            let (accepted, party) = accepted.map_or((None, None), |accepted| {
                let (word, party) = NoteAccepted::of(accepted);
                (Some(word), party)
            });
            as_payload(&TurnInterrupted {
                member: member.clone(),
                delivered,
                input_bytes: note.text.as_str().len() as u64,
                reason,
                truncated: false,
                offer: Some(offer.to_string()),
                accepted,
                party,
            })
        };
        let withdrawn = |reason: WithdrawnReason| {
            as_payload(&NoteWithdrawn {
                member: member.clone(),
                offer: offer.to_string(),
                reason,
            })
        };
        let (kind, payload) = match reply {
            Reply::Accepted(accepted) => (
                EventKind::TurnInterrupted,
                interrupted(true, None, Some(accepted)),
            ),
            Reply::Undelivered(undelivered) => (
                EventKind::TurnInterrupted,
                interrupted(false, Some(undelivered.to_string()), None),
            ),
            Reply::Expired => (
                EventKind::NoteWithdrawn,
                withdrawn(WithdrawnReason::Expired),
            ),
            Reply::Withdrawn => (
                EventKind::NoteWithdrawn,
                withdrawn(WithdrawnReason::Withdrawn),
            ),
            Reply::Admitted | Reply::Taken | Reply::Refused { .. } => return,
        };
        self.emitter.emit(kind, payload);
    }

    /// Wait up to `bound` for every offer to be answered.
    fn drained(&self, bound: Duration) {
        let held = self.lock();
        let _ = self
            .emptied
            .wait_timeout_while(held, bound, |held| !held.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// Where a member's relay is in its life: serving, asked to stop, or stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    /// Taking requests.
    #[default]
    Serving,
    /// Asked to stop; it takes no request after the one in hand.
    Stopping,
    /// It has stopped, and takes nothing more.
    Stopped,
}

/// A member's relay's [`Phase`], and the wait for it to stop.
#[derive(Default)]
struct Stop {
    phase: Mutex<Phase>,
    changed: Condvar,
}

impl Stop {
    fn lock(&self) -> MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stopping(&self) -> bool {
        *self.lock() != Phase::Serving
    }

    fn stopped(&self) {
        *self.lock() = Phase::Stopped;
        self.changed.notify_all();
    }

    /// Ask the relay to stop, and wait up to `bound` for it to.
    fn halt(&self, bound: Duration) {
        let mut phase = self.lock();
        if *phase == Phase::Serving {
            *phase = Phase::Stopping;
        }
        let _ = self
            .changed
            .wait_timeout_while(phase, bound, |phase| *phase != Phase::Stopped)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// The member's thread that carries what its spool receives into the
/// conversation's own inbox, and answers the caller with what it said.
///
/// On a thread of its own because it services a caller from another process
/// while the member's supervision loop keeps its watchdogs; and every wait it is
/// asked for runs on a thread of *its* own, because a wait blocks until the
/// conversation answers — for a supervisor-side delivery, until that party's
/// re-taken decision comes back — and the relay keeps serving other offers
/// meanwhile.
pub(crate) struct Relay {
    requests: Arc<Requests>,
    notes: onejudge::note::Notes,
    book: Arc<Book>,
    offered: Arc<AtomicU64>,
    stop: Arc<Stop>,
}

impl Relay {
    /// Open the relay for a member's `endpoint`, and the [`Ending`] its
    /// supervisor closes it with.
    ///
    /// `offered` is counted up once per note admitted — see [`Relay::admit`] — so
    /// a conversation held between turns can see a note arrive and open the turn
    /// that will take it.
    pub(crate) fn open(
        endpoint: Endpoint,
        notes: onejudge::note::Notes,
        emitter: &Emitter,
        offered: Arc<AtomicU64>,
    ) -> (Self, Ending) {
        let Endpoint {
            requests,
            spool,
            relayed,
            scratch,
        } = endpoint;
        let book = Arc::new(Book {
            scratch,
            emitter: emitter.clone(),
            relayed,
            held: Mutex::default(),
            emptied: Condvar::new(),
        });
        let _ = book.relayed.book.set(Arc::downgrade(&book));
        let stop = Arc::new(Stop::default());
        let relay = Self {
            requests: Arc::clone(&requests),
            notes,
            book: Arc::clone(&book),
            offered,
            stop: Arc::clone(&stop),
        };
        let ending = Ending {
            requests,
            spool,
            book,
            stop,
        };
        (relay, ending)
    }

    /// Serve requests until this member's spool closes or its end asks the relay
    /// to stop. The thread body.
    pub(crate) fn serve(self) {
        while !self.stop.stopping() && self.requests.closed().is_none() {
            let Some(delivered) = self.requests.take_within(RELAY_POLL) else {
                continue;
            };
            match delivered.message().clone() {
                Request::Offer { offer, note } => self.admit(offer, note, delivered),
                Request::Wait { offer } => self.answer_once_answered(&offer, None, delivered),
                Request::WaitUntil {
                    offer,
                    deadline_unix_ms,
                } => {
                    let left = Duration::from_millis(
                        deadline_unix_ms.saturating_sub(unix_millis(SystemTime::now())),
                    );
                    // The deadline arrives off the spool from another process,
                    // so one this host's monotonic clock cannot represent is
                    // read as none rather than allowed to panic the relay: it is
                    // past any instant the wait could reach.
                    self.answer_once_answered(&offer, Instant::now().checked_add(left), delivered);
                }
                Request::Withdraw { offer } => self.withdraw(&offer, delivered),
            }
        }
        self.stop.stopped();
    }

    /// Offer `note` into the conversation's inbox, and answer that it is in.
    fn admit(&self, offer: OfferId, note: Note, delivered: Delivered<Request, Reply>) {
        if self.book.knows(&offer) {
            delivered.answer(Reply::Refused {
                reason: format!("offer {offer} was already made to this member"),
            });
            return;
        }
        match self.notes.offer(note.clone()) {
            Ok(handle) => {
                // Said before the caller is answered, because during a paced
                // conversation's hold between turns the engine takes nothing
                // until the hold ends — and the hold ends on exactly this: a note
                // offered. A count rather than a flag, so two notes in one hold
                // are two arrivals rather than one.
                self.offered.fetch_add(1, Ordering::SeqCst);
                self.book.relayed.handing();
                self.book.lock().insert(
                    offer.clone(),
                    Held {
                        note: note.clone(),
                        offer: Some(handle),
                    },
                );
                // On the stream before the caller hears it is in, so nothing the
                // caller does next — and no answer the note later gets — can come
                // ahead of it.
                self.book.emitter.emit(
                    EventKind::NoteAdmitted,
                    as_payload(&NoteAdmitted {
                        member: self.book.emitter.member().unwrap_or_default().to_string(),
                        addressee: note.addressee,
                        offer: offer.to_string(),
                    }),
                );
                delivered.answer(Reply::Admitted);
            }
            // The conversation refuses a note only once it can take no more.
            Err(refused) => {
                let reply = Reply::Undelivered(Undelivered::from(refused));
                write_settled(&self.book.scratch, &offer, &reply);
                self.book.publish(&offer, &note, &reply);
                delivered.answer(reply);
            }
        }
    }

    /// Wait for `offer`'s answer — until `deadline`, if there is one — on a
    /// thread of its own, and answer the caller with it.
    fn answer_once_answered(
        &self,
        offer: &OfferId,
        deadline: Option<Instant>,
        delivered: Delivered<Request, Reply>,
    ) {
        let Some((handle, note)) = self.book.take(offer) else {
            delivered.answer(read_settled(&self.book.scratch, offer).unwrap_or_else(|| {
                Reply::Refused {
                    reason: format!(
                        "this member holds no offer {offer}, or its answer is already being \
                         waited on"
                    ),
                }
            }));
            return;
        };
        let book = Arc::clone(&self.book);
        let key = offer.clone();
        let waiting = std::thread::Builder::new().spawn(move || {
            let reply = awaited(handle, deadline);
            book.settle(&key, &note, &reply);
            delivered.answer(reply);
        });
        // llmlint: ignore-block[changed_behavior_has_e2e] a host that refused
        // this member one more thread, which no graph, task or config asks for and
        // no seam this crate sanctions fakes. The wait's closure — the in-process
        // offer and its caller's request — went with the refused spawn: the
        // caller is answered by its request being let go, the note stays offered
        // to nobody, and the book stops holding an offer nothing will settle.
        if waiting.is_err() {
            self.book.lock().remove(offer);
            self.book.relayed.answered(false);
            self.book.emptied.notify_all();
        }
        // llmlint: ignore-end[changed_behavior_has_e2e]
    }

    /// Take `offer` back, if no turn has taken it.
    fn withdraw(&self, offer: &OfferId, delivered: Delivered<Request, Reply>) {
        let Some((handle, note)) = self.book.take(offer) else {
            delivered.answer(Reply::Taken);
            return;
        };
        match handle.withdraw() {
            Ok(()) => {
                self.book.settle(offer, &note, &Reply::Withdrawn);
                delivered.answer(Reply::Withdrawn);
            }
            Err(handle) => {
                self.book.restore(offer, handle);
                delivered.answer(Reply::Taken);
            }
        }
    }
}

/// Whether a note accepted this way is carried into a worker turn the engine has
/// yet to announce — the turns [`Deliveries`] attributes.
///
/// A note the next turn opens on ([`Accepted::Queued`]), one handed over at a
/// turn's end — to the worker's next turn, or to the supervisor and on with its
/// response — and one that redirected the supervisor's decision all ride a worker
/// turn still to come. One that redirected the **worker's** live turn does not:
/// the replacement opens inside that turn, on the turn already announced, so
/// counting it would stamp the *next* turn — the supervisor's words — as a
/// delivery it never carried. Nor does one the supervisor passed the work with,
/// which no later turn carries at all.
fn opens_a_later_turn(accepted: &Accepted) -> bool {
    match accepted {
        Accepted::Queued | Accepted::AtTurnEnd { .. } => true,
        Accepted::Interrupted { party } => *party == Party::Supervisor,
        Accepted::JudgedWith { .. } => false,
    }
}

/// Block on one in-process offer, until `deadline` if there is one, and read its
/// outcome as a [`Reply`].
fn awaited(handle: onemessagebus::Offer<Accepted>, deadline: Option<Instant>) -> Reply {
    match deadline {
        None => match handle.wait() {
            Ok(accepted) => Reply::Accepted(accepted),
            Err(undelivered) => Reply::Undelivered(Undelivered::from(undelivered)),
        },
        Some(deadline) => match handle.wait_until(deadline) {
            Ok(accepted) => Reply::Accepted(accepted),
            Err(Unanswered::Expired) => Reply::Expired,
            Err(Unanswered::Undelivered(undelivered)) => {
                Reply::Undelivered(Undelivered::from(undelivered))
            }
            Err(other) => Reply::Undelivered(Undelivered::NoConversation {
                reason: other.to_string(),
            }),
        },
    }
}

/// How a member's supervisor closes its note seam.
///
/// Every half matters. The relay stops taking requests; every offer it holds is
/// answered and written down; what is still queued is answered; and the close is
/// what refuses a request that arrives after this — which a caller then answers
/// from what was written down.
pub(crate) struct Ending {
    requests: Arc<Requests>,
    /// The spool's binding, held for the conversation's life: dropping it is
    /// what unbinds the endpoint.
    spool: Spool,
    book: Arc<Book>,
    stop: Arc<Stop>,
}

impl Ending {
    /// The spool this member bound, which is what its control record names.
    pub(crate) fn endpoint(&self) -> &Path {
        self.spool.address()
    }

    /// This member takes no more notes, and `refusal` is what every later one
    /// gets — the conversation's own reason, so a caller reads *completed* and
    /// *ended* apart rather than being told only that it was too late.
    pub(crate) fn end(&self, refusal: &Undelivered) {
        self.stop.halt(STOP_BOUND);
        // Every offer nobody is waiting on is answered now, on threads of their
        // own so a member condemned with a redirect in flight cannot hold its
        // supervisor here: the conversation's answer if it gave one, and the
        // member's end if the note was never taken.
        for (offer, handle, note) in self.book.idle() {
            let book = Arc::clone(&self.book);
            let refusal = refusal.clone();
            let key = offer.clone();
            let settling = std::thread::Builder::new().spawn(move || {
                let reply = match awaited(handle, Some(Instant::now())) {
                    Reply::Expired => Reply::Undelivered(refusal),
                    reply => reply,
                };
                book.settle(&key, &note, &reply);
            });
            // llmlint: ignore-block[changed_behavior_has_e2e] a host that refused
            // this member one more thread as it ended, which no graph, task or
            // config asks for and no seam this crate sanctions fakes. The offer
            // went with the refused spawn and is answered by the close below; the
            // book stops holding it so the end is not held for it.
            if settling.is_err() {
                self.book.lock().remove(&offer);
                self.book.relayed.answered(false);
            }
            // llmlint: ignore-end[changed_behavior_has_e2e]
        }
        self.book.drained(SETTLE_BOUND);
        // What the relay had not reached is answered here rather than left for
        // its caller to time out on.
        while let Some(delivered) = self.requests.take() {
            let reply = match delivered.message().clone() {
                Request::Offer { offer, note } => {
                    let reply = Reply::Undelivered(refusal.clone());
                    write_settled(&self.book.scratch, &offer, &reply);
                    self.book.publish(&offer, &note, &reply);
                    reply
                }
                Request::Wait { offer }
                | Request::WaitUntil { offer, .. }
                | Request::Withdraw { offer } => read_settled(&self.book.scratch, &offer)
                    .unwrap_or_else(|| Reply::Undelivered(refusal.clone())),
            };
            delivered.answer(reply);
        }
        self.requests.close(Closed::from(refusal));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Labels;

    /// Where a relay's events land, for a test to read back.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Recorder {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Recorder {
        /// Every envelope of `kind` written so far.
        fn of_kind(&self, kind: &str) -> Vec<serde_json::Value> {
            let written = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            String::from_utf8_lossy(&written)
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|event| event["kind"] == kind)
                .collect()
        }
    }

    /// An emitter labelled for one member, writing into `recorder`.
    fn emitter(recorder: &Recorder) -> Emitter {
        Emitter::new("stream", Box::new(recorder.clone())).with_labels(Labels {
            member: Some("worker".to_string()),
            ..Labels::default()
        })
    }

    fn note(addressee: Addressee) -> Note {
        Note::new(addressee, "the migration has to be reversible").expect("a note")
    }

    /// One member's seam, bound in `scratch` and relaying into a **real**
    /// conversation inbox the test plays the engine's part on.
    struct Member {
        ending: Ending,
        relayed: Arc<Relayed>,
        conversation: onejudge::note::NoteInbox,
        recorder: Recorder,
        offered: Arc<AtomicU64>,
    }

    fn member(scratch: &Path) -> Member {
        let endpoint = Endpoint::bind(scratch).expect("an endpoint");
        let relayed = endpoint.relayed();
        let (notes, conversation) = onejudge::note::Notes::channel();
        let recorder = Recorder::default();
        let offered = Arc::new(AtomicU64::new(0));
        let (relay, ending) =
            Relay::open(endpoint, notes, &emitter(&recorder), Arc::clone(&offered));
        std::thread::spawn(move || relay.serve());
        Member {
            ending,
            relayed,
            conversation,
            recorder,
            offered,
        }
    }

    fn admitted(scratch: &Path, note: &Note) -> NoteOffer {
        match offer(scratch, note) {
            NoteOffered::Admitted(offer) => offer,
            NoteOffered::Undelivered(undelivered) => panic!("not admitted: {undelivered}"),
        }
    }

    /// An offer is answered `Admitted` once the note is in the conversation's
    /// inbox and published as such — and its wait reads the conversation's own
    /// answer, published once, written down, and counted.
    #[test]
    fn an_offer_is_admitted_before_it_is_answered_and_its_wait_reads_the_conversations_answer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let member = member(dir.path());
        let offer = admitted(dir.path(), &note(Addressee::Worker));
        let id = offer.id().to_string();

        let admissions = member.recorder.of_kind("note-admitted");
        assert_eq!(admissions.len(), 1, "admission was not published once");
        assert_eq!(admissions[0]["payload"]["offer"], id.as_str());
        assert_eq!(admissions[0]["payload"]["addressee"], "worker");
        assert_eq!(member.offered.load(Ordering::SeqCst), 1);
        assert!(member.recorder.of_kind("turn-interrupted").is_empty());

        let delivered = member
            .conversation
            .take_within(Duration::from_secs(10))
            .expect("the note is in the conversation's inbox");
        assert_eq!(delivered.message(), &note(Addressee::Worker));
        delivered.answer(Accepted::Queued);

        assert_eq!(
            offer.wait().expect("a wait answers"),
            NoteDelivery::Accepted(Accepted::Queued)
        );
        let answered = member.recorder.of_kind("turn-interrupted");
        assert_eq!(answered.len(), 1);
        assert_eq!(answered[0]["payload"]["offer"], id.as_str());
        assert_eq!(answered[0]["payload"]["accepted"], "queued");
        assert_eq!(answered[0]["payload"]["delivered"], true);
        assert_eq!(member.relayed.taken(Duration::ZERO), 1);
        let offer_id = OfferId::try_from(id).expect("a minted id is one");
        assert_eq!(
            read_settled(dir.path(), &offer_id),
            Some(Reply::Accepted(Accepted::Queued)),
            "the answer was not written down before it was given"
        );
    }

    /// A note no turn takes before its deadline **expires**: withdrawn by the
    /// bus in the same moment, so the conversation never finds it, and published
    /// as `note-withdrawn` with reason `expired`.
    #[test]
    fn a_note_untaken_at_its_deadline_expires_and_the_conversation_never_finds_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let member = member(dir.path());
        let offer = admitted(dir.path(), &note(Addressee::Worker));
        let id = offer.id().to_string();
        assert_eq!(
            offer
                .wait_until(Instant::now() + Duration::from_millis(200))
                .expect("a wait answers"),
            NoteDelivery::Expired
        );
        assert!(
            member.conversation.take().is_none(),
            "an expired note was still in the conversation's inbox"
        );
        let withdrawn = member.recorder.of_kind("note-withdrawn");
        assert_eq!(withdrawn.len(), 1);
        assert_eq!(withdrawn[0]["payload"]["offer"], id.as_str());
        assert_eq!(withdrawn[0]["payload"]["reason"], "expired");
        assert_eq!(member.relayed.taken(Duration::ZERO), 0);
    }

    /// A deadline off the spool as late as the wire can name is a wait with no
    /// deadline, never a relay that dies converting it: the conversation's answer
    /// still arrives, and the relay still serves the next request.
    #[test]
    fn a_deadline_past_any_clock_waits_like_no_deadline_and_the_relay_lives_on() {
        let dir = tempfile::tempdir().expect("tempdir");
        let member = member(dir.path());
        let offer = admitted(dir.path(), &note(Addressee::Worker));
        let id = OfferId::try_from(offer.id().to_string()).expect("a minted id is one");
        let scratch = dir.path().to_path_buf();
        let asked = {
            let id = id.clone();
            std::thread::spawn(move || {
                ask(
                    &scratch,
                    Request::WaitUntil {
                        offer: id,
                        deadline_unix_ms: u64::MAX,
                    },
                )
            })
        };
        let delivered = member
            .conversation
            .take_within(Duration::from_secs(10))
            .expect("the note is in the conversation's inbox");
        delivered.answer(Accepted::Queued);
        assert_eq!(
            asked.join().expect("the asking thread"),
            Ok(Reply::Accepted(Accepted::Queued))
        );

        let next = admitted(dir.path(), &note(Addressee::Worker));
        next.withdraw()
            .expect("the relay still serves a withdrawal");
    }

    /// The note protocol's wire forms and the written-down answer, held to a
    /// committed golden rather than only to a round trip through today's types:
    /// a member started by an older build answers a caller from this one over
    /// these bytes, and a late caller reads an answer an older member wrote, so a
    /// change to either shape under an unchanged schema version fails here.
    /// Each golden value must read as its type and write back byte-for-byte, and
    /// the golden must carry every request and reply variant — the matches below
    /// are exhaustive, so a new variant cannot ship without a golden entry.
    #[test]
    fn the_note_wire_and_its_written_down_answer_match_the_committed_golden() {
        fn request_tag(request: &Request) -> &'static str {
            match request {
                Request::Offer { .. } => "offer",
                Request::Wait { .. } => "wait",
                Request::WaitUntil { .. } => "wait_until",
                Request::Withdraw { .. } => "withdraw",
            }
        }
        fn reply_tag(reply: &Reply) -> &'static str {
            match reply {
                Reply::Admitted => "admitted",
                Reply::Accepted(_) => "accepted",
                Reply::Undelivered(_) => "undelivered",
                Reply::Expired => "expired",
                Reply::Withdrawn => "withdrawn",
                Reply::Taken => "taken",
                Reply::Refused { .. } => "refused",
            }
        }
        let golden: serde_json::Value =
            serde_json::from_str(include_str!("../tests/golden/note-wire.v1.json"))
                .expect("the golden is JSON");
        let mut requests = std::collections::BTreeSet::new();
        for value in golden["requests"].as_array().expect("requests") {
            let request: Request = serde_json::from_value(value.clone()).expect("a request");
            assert_eq!(&serde_json::to_value(&request).expect("serializes"), value);
            requests.insert(request_tag(&request));
        }
        assert_eq!(
            requests.into_iter().collect::<Vec<_>>(),
            ["offer", "wait", "wait_until", "withdraw"]
        );
        let mut replies = std::collections::BTreeSet::new();
        for value in golden["replies"].as_array().expect("replies") {
            let reply: Reply = serde_json::from_value(value.clone()).expect("a reply");
            assert_eq!(&serde_json::to_value(&reply).expect("serializes"), value);
            replies.insert(reply_tag(&reply));
        }
        assert_eq!(
            replies.into_iter().collect::<Vec<_>>(),
            [
                "accepted",
                "admitted",
                "expired",
                "refused",
                "taken",
                "undelivered",
                "withdrawn"
            ]
        );
        for value in golden["settled"].as_array().expect("settled") {
            let settled: Settled = serde_json::from_value(value.clone()).expect("a settled");
            assert_eq!(settled.schema_version, SETTLED_SCHEMA_VERSION);
            assert_eq!(&serde_json::to_value(&settled).expect("serializes"), value);
        }
        assert_eq!(Request::SCHEMA.to_string(), "oneagentgraph.note-request@1");
    }

    /// A note withdrawn before any turn takes it is gone — and one a turn has
    /// taken cannot be withdrawn: the offer comes back to be waited on, and a
    /// note that redirected the worker's own live turn is not counted as one a
    /// later turn carries.
    #[test]
    fn a_withdrawal_succeeds_only_for_a_note_no_turn_has_taken() {
        let dir = tempfile::tempdir().expect("tempdir");
        let member = member(dir.path());

        let untaken = admitted(dir.path(), &note(Addressee::Worker));
        let id = untaken.id().to_string();
        untaken.withdraw().expect("an untaken note withdraws");
        assert!(member.conversation.take().is_none());
        let withdrawn = member.recorder.of_kind("note-withdrawn");
        assert_eq!(withdrawn.len(), 1);
        assert_eq!(withdrawn[0]["payload"]["offer"], id.as_str());
        assert_eq!(withdrawn[0]["payload"]["reason"], "withdrawn");

        let taken = admitted(dir.path(), &note(Addressee::Worker));
        let delivered = member
            .conversation
            .take_within(Duration::from_secs(10))
            .expect("the second note is in the inbox");
        let taken = taken
            .withdraw()
            .expect_err("a note a turn has taken was withdrawn");
        delivered.answer(Accepted::Interrupted {
            party: Party::Worker,
        });
        assert_eq!(
            taken.wait().expect("a wait answers"),
            NoteDelivery::Accepted(Accepted::Interrupted {
                party: Party::Worker
            })
        );
        assert_eq!(
            member.relayed.taken(Duration::ZERO),
            0,
            "a note that redirected the live worker turn was counted for a later one"
        );
    }

    /// Every acceptance survives the spool and relay to the caller unchanged, and
    /// is counted for a later worker turn exactly when one carries it.
    #[test]
    fn every_disposition_survives_the_relay_and_is_counted_only_where_a_later_turn_carries_it() {
        let dispositions = [
            (Accepted::Queued, true),
            (
                Accepted::Interrupted {
                    party: Party::Worker,
                },
                false,
            ),
            (
                Accepted::Interrupted {
                    party: Party::Supervisor,
                },
                true,
            ),
            (
                Accepted::AtTurnEnd {
                    party: Party::Worker,
                },
                true,
            ),
            (
                Accepted::AtTurnEnd {
                    party: Party::Supervisor,
                },
                true,
            ),
            (
                Accepted::JudgedWith {
                    completion_reason: "passed with the note in hand".to_string(),
                },
                false,
            ),
        ];
        let dir = tempfile::tempdir().expect("tempdir");
        let member = member(dir.path());
        let mut counted = 0;
        for (disposition, carried) in &dispositions {
            let offer = admitted(dir.path(), &note(Addressee::Both));
            member
                .conversation
                .take_within(Duration::from_secs(10))
                .expect("a note reaches the conversation")
                .answer(disposition.clone());
            assert_eq!(
                offer.wait().expect("a wait answers"),
                NoteDelivery::Accepted(disposition.clone()),
                "{disposition:?} did not survive the relay"
            );
            counted += usize::from(*carried);
            assert_eq!(
                member.relayed.taken(Duration::ZERO),
                counted,
                "{disposition:?} was counted wrongly"
            );
        }
    }

    /// A member that ends answers every offer it holds before its spool closes,
    /// and writes each answer down — so a caller asking after it has gone reads
    /// what became of its note rather than a refusal. An offer made after the end
    /// is refused with the end's own reason.
    #[test]
    fn a_member_that_ends_answers_every_offer_it_holds_and_a_late_caller_reads_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let member = member(dir.path());
        let answered = admitted(dir.path(), &note(Addressee::Worker));
        let untaken = admitted(dir.path(), &note(Addressee::Worker));
        member
            .conversation
            .take_within(Duration::from_secs(10))
            .expect("the first note reaches the conversation")
            .answer(Accepted::Queued);
        let refusal = Undelivered::ConversationCompleted {
            completion_reason: "its supervisor judged the task complete".to_string(),
        };
        member.ending.end(&refusal);

        assert_eq!(
            answered.wait().expect("a wait answers"),
            NoteDelivery::Accepted(Accepted::Queued),
            "a note the conversation answered was reported otherwise once the member ended"
        );
        assert_eq!(
            untaken.wait().expect("a wait answers"),
            NoteDelivery::Undelivered(refusal.clone()),
            "a note nothing took was not refused with the member's end"
        );
        match offer(dir.path(), &note(Addressee::Worker)) {
            NoteOffered::Undelivered(late) => assert_eq!(late, refusal),
            NoteOffered::Admitted(_) => panic!("a member that ended admitted a note"),
        }
        assert_eq!(member.ending.endpoint(), dir.path().join(NOTES_DIR));
    }

    /// An answer the member cannot write down is still given to the caller that
    /// is waiting; only a caller that asks after the member has gone loses it, and
    /// is answered from the spool's close instead.
    #[test]
    fn an_unwritable_answer_store_still_answers_the_live_caller() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A file where the store's directory goes: every write into it fails.
        std::fs::write(dir.path().join(OFFERS_DIR), "occupied").expect("occupy the store");
        let member = member(dir.path());
        let live = admitted(dir.path(), &note(Addressee::Worker));
        let late = admitted(dir.path(), &note(Addressee::Worker));
        let id = OfferId::try_from(late.id().to_string()).expect("a minted id");
        member
            .conversation
            .take_within(Duration::from_secs(10))
            .expect("the live note reaches the conversation")
            .answer(Accepted::Queued);
        assert_eq!(
            live.wait().expect("a wait answers"),
            NoteDelivery::Accepted(Accepted::Queued),
            "a caller waiting was not answered because the answer could not be written"
        );
        let refusal = Undelivered::MemberSettled {
            outcome: "the member was condemned by its activity watchdog".to_string(),
        };
        member.ending.end(&refusal);
        assert_eq!(read_settled(dir.path(), &id), None);
        assert_eq!(
            late.wait().expect("a wait answers"),
            NoteDelivery::Undelivered(refusal),
            "a late caller was not answered from the member's close"
        );
    }

    /// A member with no endpoint is reported rather than waited on; and an
    /// endpoint nothing has bound is waited on only as long as the bus's bound,
    /// with the note taken back on the way out.
    #[test]
    fn a_member_that_never_takes_a_note_is_reported_rather_than_waited_on() {
        let dir = tempfile::tempdir().expect("tempdir");
        match offer(dir.path(), &note(Addressee::Worker)) {
            NoteOffered::Undelivered(Undelivered::NoConversation { reason }) => {
                assert!(reason.contains("binds no note endpoint"), "{reason}");
            }
            other => panic!("a member with no endpoint was answered {other:?}"),
        }

        std::fs::create_dir(dir.path().join(NOTES_DIR)).expect("an unbound spool");
        let silent = submit_within(
            dir.path(),
            &note(Addressee::Worker),
            Duration::from_millis(50),
        )
        .expect_err("a spool nothing is serving cannot take a note");
        assert!(
            matches!(&silent, Undelivered::NoConversation { reason }
                if reason.contains("was withdrawn")),
            "{silent:?}"
        );
        let left: Vec<_> = std::fs::read_dir(dir.path().join(NOTES_DIR))
            .expect("the spool")
            .flatten()
            .collect();
        assert!(left.is_empty(), "a refused note was left in the spool");
    }

    /// An endpoint a build before the offer protocol bound takes a bare note and
    /// answers it once: the note is still delivered, answered when it is, and
    /// there is nothing to take back.
    #[test]
    fn an_endpoint_that_takes_a_bare_note_is_still_delivered_to() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inbox = onejudge::note::NoteInbox::new();
        let _spool = Spool::bind(dir.path().join(NOTES_DIR), &inbox).expect("a bare-note spool");
        let conversation = std::thread::spawn(move || {
            inbox
                .take_within(Duration::from_secs(10))
                .expect("the note reaches the conversation")
                .answer(Accepted::AtTurnEnd {
                    party: Party::Worker,
                });
            inbox
        });
        let offer = admitted(dir.path(), &note(Addressee::Worker));
        let _inbox = conversation.join().expect("the conversation");
        let offer = offer
            .withdraw()
            .expect_err("an answered note cannot be taken back");
        assert_eq!(
            offer.wait().expect("a wait answers"),
            NoteDelivery::Accepted(Accepted::AtTurnEnd {
                party: Party::Worker
            })
        );
    }

    /// An offer id is checked as it arrives — it names a file in the member's
    /// scratch — and every id this crate mints passes.
    #[test]
    fn an_offer_id_is_refused_unless_it_is_a_safe_file_name() {
        for minted in [OfferId::mint(), OfferId::mint()] {
            assert!(OfferId::try_from(minted.0.clone()).is_ok(), "{minted}");
        }
        assert_ne!(OfferId::mint(), OfferId::mint(), "two offers shared an id");
        for hostile in [
            String::new(),
            "../../escape".to_string(),
            "-leading".to_string(),
            "has space".to_string(),
            "x".repeat(MAX_OFFER_ID + 1),
        ] {
            assert!(OfferId::try_from(hostile.clone()).is_err(), "{hostile:?}");
            assert!(
                serde_json::from_value::<Request>(serde_json::json!({
                    "withdraw": {"offer": hostile}
                }))
                .is_err(),
                "a request naming {hostile:?} was read"
            );
        }
    }

    /// A turn announced while a note is with the conversation waits for its
    /// answer, so a note taken just before the announcement is attributed to the
    /// turn that carries it — and one still unanswered past the bound is not.
    #[test]
    fn a_turn_waits_for_a_note_already_handed_over_and_attributes_it_once() {
        let relayed = Arc::new(Relayed::default());
        let mut deliveries = Deliveries::of(Some(Arc::clone(&relayed)));
        assert!(
            !deliveries.carried_by_this_turn(),
            "nothing was handed over"
        );

        relayed.handing();
        let answering = {
            let relayed = Arc::clone(&relayed);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                relayed.answered(true);
            })
        };
        assert!(
            deliveries.carried_by_this_turn(),
            "the turn did not wait for the note the conversation had taken"
        );
        answering.join().expect("the answering thread");
        assert!(
            !deliveries.carried_by_this_turn(),
            "one note was attributed to two turns"
        );

        // A member with no seam attributes nothing.
        assert!(!Deliveries::of(None).carried_by_this_turn());
    }

    /// A member with no conversation is handed the addressed role too.
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
