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
//! carries a [`Request`] at a time: offer this note, wait for that offer's answer
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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
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
/// carries it, and the relay learns that answer on its own thread a moment later
/// — so the wait is that moment, and this bound is only ever reached by a note
/// that arrived after the engine last looked, which the announced turn does not
/// carry.
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
    id: String,
    state: OfferState,
}

#[derive(Debug)]
enum OfferState {
    /// Held by the member's relay, reached through its spool.
    Relayed { scratch: PathBuf, offer: OfferId },
    /// Answered already, by a member that has no offer to hold: a run started by
    /// a build whose endpoint took a bare note, or a single-sided member whose one
    /// lever answered at once. There is nothing to take back.
    Answered(NoteDelivery),
}

impl NoteOffer {
    /// An offer whose answer is already known, with nothing to take back.
    pub(crate) fn answered(delivery: NoteDelivery) -> Self {
        Self {
            id: OfferId::mint().0,
            state: OfferState::Answered(delivery),
        }
    }

    /// This offer's id: unique per offer within the run, and the id the run's
    /// `note-admitted`, `turn-interrupted` and `note-withdrawn` events name it by.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
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
            OfferState::Relayed { scratch, offer } => {
                let asked = ask(
                    &scratch,
                    Request::Wait {
                        offer: offer.clone(),
                    },
                );
                resolve(&scratch, &offer, asked)
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
            OfferState::Relayed { scratch, offer } => {
                // A remaining span rather than an instant, because an `Instant`
                // means nothing to another process: the member reads it on its
                // own clock the moment it receives it, which is as early as it
                // could act on the deadline at all.
                let within = deadline.saturating_duration_since(Instant::now());
                let asked = ask(
                    &scratch,
                    Request::WaitUntil {
                        offer: offer.clone(),
                        within_ms: u64::try_from(within.as_millis()).unwrap_or(u64::MAX),
                    },
                );
                resolve(&scratch, &offer, asked)
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
        let OfferState::Relayed { scratch, offer } = &self.state else {
            return Err(self);
        };
        match ask(
            scratch,
            Request::Withdraw {
                offer: offer.clone(),
            },
        ) {
            Ok(Reply::Withdrawn) => Ok(()),
            _ => Err(self),
        }
    }
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
    /// As [`Request::Wait`], withdrawing the note if it is still untaken
    /// `within_ms` from when the member reads this.
    WaitUntil {
        /// Which offer.
        offer: OfferId,
        /// How long from now the deadline is.
        within_ms: u64,
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
#[serde(rename_all = "snake_case")]
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
    if Spool::declared(&address).ok().flatten() == Some(Note::SCHEMA) {
        return match submit(scratch, note) {
            Ok(accepted) => {
                NoteOffered::Admitted(NoteOffer::answered(NoteDelivery::Accepted(accepted)))
            }
            Err(undelivered) => NoteOffered::Undelivered(undelivered),
        };
    }
    let offer = OfferId::mint();
    match ask(
        scratch,
        Request::Offer {
            offer: offer.clone(),
            note: note.clone(),
        },
    ) {
        Ok(Reply::Admitted) => NoteOffered::Admitted(NoteOffer {
            id: offer.0.clone(),
            state: OfferState::Relayed {
                scratch: scratch.to_path_buf(),
                offer,
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
/// caller that asks after the member has gone loses it.
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
#[derive(Debug, Default)]
pub(crate) struct Relayed {
    state: Mutex<Handed>,
    answered: Condvar,
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
    fn taken(&self, within: Duration) -> usize {
        let state = self.lock();
        let (state, _) = self
            .answered
            .wait_timeout_while(state, within, |state| state.in_flight > 0)
            .unwrap_or_else(PoisonError::into_inner);
        state.taken
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
        self.publish(offer, note, reply);
        self.relayed.answered(match reply {
            Reply::Accepted(accepted) => opens_a_later_turn(accepted),
            _ => false,
        });
        self.lock().remove(offer);
        self.emptied.notify_all();
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

/// Whether a member's relay has been asked to stop, and whether it has.
#[derive(Default)]
struct Stop {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl Stop {
    fn lock(&self) -> MutexGuard<'_, (bool, bool)> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stopping(&self) -> bool {
        self.lock().0
    }

    fn stopped(&self) {
        self.lock().1 = true;
        self.changed.notify_all();
    }

    /// Ask the relay to stop, and wait up to `bound` for it to.
    fn halt(&self, bound: Duration) {
        let mut state = self.lock();
        state.0 = true;
        let _ = self
            .changed
            .wait_timeout_while(state, bound, |state| !state.1)
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
                Request::WaitUntil { offer, within_ms } => {
                    let deadline = Instant::now() + Duration::from_millis(within_ms);
                    self.answer_once_answered(&offer, Some(deadline), delivered);
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
            let _ = std::thread::Builder::new().spawn(move || {
                let reply = match awaited(handle, Some(Instant::now())) {
                    Reply::Expired => Reply::Undelivered(refusal),
                    reply => reply,
                };
                book.settle(&offer, &note, &reply);
            });
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
