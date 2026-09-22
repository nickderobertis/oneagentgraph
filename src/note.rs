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
//! [`onejudge::note`], which declares them — the note contract of the two-party
//! conversation that receives them, the message `agent.note@1`. Nothing about
//! them is declared here, and that is deliberate — a second declaration is a
//! shape that drifts, and a note that satisfies the copy is still refused by the
//! conversation it was written for.
//!
//! # The routing is onejudge's
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
//! # The transport is the bus core's
//!
//! A note is offered by a *different process* from the one running the member —
//! `oneagentgraph`'s own API, against a run's state directory. The transport
//! between the two is the bus inbox's [`Spool`] backend: the member binds an
//! [`onejudge::note::NoteInbox`] of its own over a spool in its scratch
//! (`Endpoint`), the spool's own courier moves each offered note into that
//! inbox, and the caller's end is the bus [`onemessagebus::Sender`] over the
//! spool's address ([`submit`]).
//!
//! # What is this crate's: the relay
//!
//! What stays here is one thread of the member's process, the `Relay`, that
//! takes each note out of the member's inbox, hands it to the conversation's, and
//! answers the caller with what the conversation said. It is a thread rather than
//! the spool bound straight onto the engine's inbox for the three things only the
//! member process can do with a disposition: publish it on the run's own stream,
//! count the note as offered so a paced conversation holding between turns opens
//! the turn that will take it, and keep the record `Deliveries` attributes a
//! turn from.
//!
//! On a thread of its own, and that is load-bearing: `send` blocks until the
//! note's disposition is known — for the supervisor, until its re-taken decision
//! comes back — so servicing the inbox from the supervision loop would stall the
//! watchdogs behind a judge invocation.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use onejudge::note::NoteInbox;
use onemessagebus::{Closed, SPOOL_WAIT};

use crate::event::{Emitter, EventKind, TurnInterrupted};
use crate::member::as_payload;

pub use onejudge::note::{
    Accepted, Addressee, Criterion, DeliveredNote, Note, NoteRefused, NoteText, Party, Undelivered,
};
pub use onemessagebus::Spool;

/// The directory a two-party member binds inside its own scratch to receive
/// notes for the conversation's life — a sibling of
/// [`crate::control::CONTROL_FILE`].
pub const NOTES_DIR: &str = "notes";

/// The shape every document in a member's spool is written under: the bus
/// spool's own, refused by number when a build that knew more wrote it.
pub const NOTE_SCHEMA_VERSION: u32 = onemessagebus::SPOOL_SCHEMA_VERSION;

/// How often the [`Relay`] looks for a note when none is waiting, which is also
/// how soon it notices the member's inbox has closed.
const RELAY_POLL: Duration = Duration::from_millis(50);

/// How long a worker turn being announced waits for a note already handed to the
/// conversation to be answered, before it is announced without one.
///
/// The engine answers a note it takes *before* it announces the turn that
/// carries it, and the relay learns that answer on its own thread a moment later
/// — so the wait is that moment, and this bound is only ever reached by a note
/// that arrived after the engine last looked, which the announced turn does not
/// carry.
const DELIVERY_SETTLE: Duration = Duration::from_secs(1);

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

/// Hand `note` to the conversation running in `scratch`, and answer what became
/// of it.
///
/// The caller's end of the seam: the bus [`onemessagebus::Sender`] over the
/// member's spool, whose own courier hands the note to the member's inbox. What
/// comes back is the conversation's own answer, not a guess made from outside.
///
/// # Errors
///
/// [`Undelivered`], for a note the conversation did not take: it had already
/// completed, it had ended, or nothing ever took it.
pub fn submit(scratch: &Path, note: &Note) -> Result<Accepted, Undelivered> {
    submit_within(scratch, note, SPOOL_WAIT)
}

/// [`submit`], with the wait made a parameter so a test can drive the deadline
/// itself rather than sitting out the real one.
fn submit_within(scratch: &Path, note: &Note, wait: Duration) -> Result<Accepted, Undelivered> {
    let address = scratch.join(NOTES_DIR);
    // Said in this crate's words rather than the backend's, because the caller
    // asked about a member rather than a directory.
    if !address.is_dir() {
        return Err(Undelivered::NoConversation {
            reason: "this member binds no note endpoint: only a two-party member's own thread \
                     does, and this run recorded none for it"
                .to_string(),
        });
    }
    Spool::connect_within::<Note, Accepted>(address, wait)
        .send(note.clone())
        .map_err(Undelivered::from)
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

/// Where one member receives notes: its own inbox, bound over a spool in its
/// scratch for the conversation's life.
///
/// A directory rather than a socket, and that is the one deviation from the
/// approved contract's wording: the record still names the endpoint by path, and
/// the two ends still meet nowhere else, but a member of this crate runs on
/// Windows too — where a unix domain socket is exactly why [`crate::control`]
/// already reports *no controllable turn* — and a note seam that existed on one
/// platform only would be a delivery an operator could not rely on.
pub(crate) struct Endpoint {
    inbox: Arc<NoteInbox>,
    spool: Spool,
    relayed: Arc<Relayed>,
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
        let inbox = NoteInbox::new();
        let spool = Spool::bind(scratch.join(NOTES_DIR), &inbox).ok()?;
        Some(Self {
            inbox: Arc::new(inbox),
            spool,
            relayed: Arc::default(),
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
    /// Notes the conversation accepted.
    taken: usize,
}

impl Relayed {
    fn handing(&self) {
        self.lock().in_flight += 1;
    }

    fn answered(&self, accepted: bool) {
        let mut state = self.lock();
        state.in_flight -= 1;
        if accepted {
            state.taken += 1;
        }
        drop(state);
        self.answered.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Handed> {
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

/// The member's thread that carries what its inbox receives into the
/// conversation's own inbox, and answers the caller with what it said.
///
/// On a thread of its own because [`onejudge::note::Notes`]' `send` blocks until
/// the note's disposition is known — for a supervisor-side delivery, until that
/// party's re-taken decision comes back. Servicing the inbox from the supervision
/// loop would put a judge invocation between two heartbeats.
pub(crate) struct Relay {
    inbox: Arc<NoteInbox>,
    notes: onejudge::note::Notes,
    emitter: Emitter,
    offered: Arc<AtomicU64>,
    relayed: Arc<Relayed>,
}

impl Relay {
    /// Open the relay for a member's `endpoint`, and the [`Ending`] its
    /// supervisor closes it with.
    ///
    /// `offered` is counted up once per note, *before* the relay blocks handing
    /// it over — see [`Relay::serve`] — so a conversation held between turns can
    /// see a note arrive and open the turn that will take it.
    pub(crate) fn open(
        endpoint: Endpoint,
        notes: onejudge::note::Notes,
        emitter: &Emitter,
        offered: Arc<AtomicU64>,
    ) -> (Self, Ending) {
        let Endpoint {
            inbox,
            spool,
            relayed,
        } = endpoint;
        let relay = Self {
            inbox: Arc::clone(&inbox),
            notes,
            emitter: emitter.clone(),
            offered,
            relayed,
        };
        let ending = Ending {
            inbox,
            spool,
            emitter: emitter.clone(),
        };
        (relay, ending)
    }

    /// Carry notes until this member's inbox closes. The thread body.
    pub(crate) fn serve(self) {
        while self.inbox.closed().is_none() {
            let Some(delivered) = self.inbox.take_within(RELAY_POLL) else {
                continue;
            };
            let note = delivered.message().clone();
            // Said before the hand-over blocks, because during a paced
            // conversation's hold between turns the engine disposes of nothing
            // until the hold ends — and the hold ends on exactly this: a note
            // offered. A count rather than a flag, so two notes in one hold are
            // two arrivals rather than one.
            self.offered.fetch_add(1, Ordering::SeqCst);
            self.relayed.handing();
            // Blocks: the conversation is what decides, and for a note that
            // reaches the supervisor's live turn the decision *is* the answer.
            let answer = self.notes.send(note.clone()).map_err(Undelivered::from);
            self.relayed.answered(answer.is_ok());
            match answer {
                Ok(accepted) => {
                    publish(
                        &self.emitter,
                        &note,
                        &NoteDelivery::Accepted(accepted.clone()),
                    );
                    delivered.answer(accepted);
                }
                // The conversation refuses a note only once it can take no more,
                // so its refusal is every later note's too: closing with it
                // answers this caller and each one after in the conversation's
                // own words.
                Err(refusal) => {
                    publish(
                        &self.emitter,
                        &note,
                        &NoteDelivery::Undelivered(refusal.clone()),
                    );
                    self.inbox.close(Closed::from(&refusal));
                }
            }
        }
    }
}

/// How a member's supervisor closes its note seam.
///
/// Both halves matter. The close is what refuses a note that arrives after this,
/// and the notes already waiting are answered here rather than left as an
/// acceptance nobody reads.
pub(crate) struct Ending {
    inbox: Arc<NoteInbox>,
    /// The spool's binding, held for the conversation's life: dropping it is
    /// what unbinds the endpoint.
    spool: Spool,
    emitter: Emitter,
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
        // Anything the relay had not reached is published here, and answered by
        // the close below, rather than left for its caller to time out on.
        let waiting: Vec<_> = std::iter::from_fn(|| self.inbox.take()).collect();
        for delivered in &waiting {
            publish(
                &self.emitter,
                delivered.message(),
                &NoteDelivery::Undelivered(refusal.clone()),
            );
        }
        self.inbox.close(Closed::from(refusal));
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
            truncated: false,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Labels;

    /// An emitter that writes nowhere, labelled for one member — what a relay
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

    /// A member's endpoint, with its relay running into `notes` and the ending
    /// its supervisor would hold.
    fn relaying(scratch: &Path, notes: onejudge::note::Notes) -> (Ending, Arc<Relayed>) {
        let endpoint = Endpoint::bind(scratch).expect("an endpoint");
        let relayed = endpoint.relayed();
        let (relay, ending) = Relay::open(
            endpoint,
            notes,
            &emitter("worker"),
            Arc::new(AtomicU64::new(0)),
        );
        std::thread::spawn(move || relay.serve());
        (ending, relayed)
    }

    /// A note offered with **no turn open** is held for the next turn, and a note
    /// offered once the conversation can take no more is refused — both answered
    /// by the conversation itself, through the bus spool and this crate's relay.
    ///
    /// That round trip is what this crate owns here. The routing is onejudge's,
    /// and `tests/e2e/note.rs` drives the live-turn deliveries against a real
    /// conversation; what is proven here is that a note sent into a member's
    /// spool by one process reaches the conversation's inbox unchanged and that
    /// the answer the conversation gives is what the caller reads, rather than
    /// anything this crate decided for itself.
    ///
    /// Both dispositions are driven against a **real** conversation inbox rather
    /// than a stand-in: one a receiver answers `queued` from, which is what the
    /// engine answers a note offered with no live turn, and one closed with the
    /// engine's own refusal for a channel no turn ever opened on.
    #[test]
    fn a_note_with_no_live_turn_is_held_for_the_next_one_and_one_too_late_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (notes, inbox) = onejudge::note::Notes::channel();
        let (_ending, relayed) = relaying(dir.path(), notes);
        let conversation = std::thread::spawn(move || {
            let delivered = inbox
                .take_within(Duration::from_secs(10))
                .expect("the note reaches the conversation");
            assert_eq!(
                delivered.message(),
                &note(Addressee::Worker),
                "the note changed in transit"
            );
            delivered.answer(Accepted::Queued);
            inbox.close(Closed::from(&Undelivered::NoConversation {
                reason: "the conversation's note inbox was dropped before any turn opened".into(),
            }));
        });

        assert_eq!(
            submit(dir.path(), &note(Addressee::Worker)),
            Ok(Accepted::Queued),
            "a note offered with no live turn was not held for the next one"
        );
        conversation.join().expect("the conversation");
        assert_eq!(relayed.taken(Duration::ZERO), 1);

        // And once nothing will read the channel again, a note is refused rather
        // than accepted into a member nobody will take it out of.
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
        // A refused note is never counted as one a turn carried.
        assert_eq!(relayed.taken(Duration::ZERO), 1);
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
            // Nothing relays: the note waits in the member's inbox until the
            // conversation ends, which is what answers it.
            let endpoint = Endpoint::bind(dir.path()).expect("an endpoint");
            let ending = Ending {
                inbox: Arc::clone(&endpoint.inbox),
                spool: endpoint.spool,
                emitter: emitter("worker"),
            };
            let waiting = {
                let scratch = dir.path().to_path_buf();
                std::thread::spawn(move || submit(&scratch, &note(Addressee::Worker)))
            };
            // The courier has moved it into the inbox before the end answers it.
            let taken = (0..400).any(|_| {
                std::thread::sleep(Duration::from_millis(10));
                !format!("{:?}", endpoint.inbox).contains("queued: 0")
            });
            assert!(taken, "the spool never moved the note into the inbox");
            ending.end(&refusal);
            assert_eq!(
                waiting.join().expect("the waiting caller"),
                Err(refusal.clone()),
                "a note already in the member's inbox was not answered by the end of the conversation"
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
            assert_eq!(ending.endpoint(), dir.path().join(NOTES_DIR));
        }
    }

    /// A member with no endpoint, and a spool nothing has bound, are both
    /// reported rather than waited on forever.
    ///
    /// The second is the one that matters: a caller blocked on a member that
    /// never took its note is a caller that never learns it did not land, which
    /// is the same silence the whole seam exists to replace. The note is taken
    /// back on the way out, so a member that binds later does not deliver a note
    /// its caller was already told about.
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
        assert!(
            left.is_empty(),
            "a note its caller was told about was left in the spool for a member to deliver later"
        );
    }

    /// Every disposition a conversation can answer crosses the member's spool
    /// and relay to the caller unchanged.
    #[test]
    fn every_disposition_survives_the_spool_unchanged() {
        let dispositions = [
            Accepted::Queued,
            Accepted::Interrupted {
                party: Party::Worker,
            },
            Accepted::Interrupted {
                party: Party::Supervisor,
            },
            Accepted::JudgedWith {
                completion_reason: "the supervisor passed it with the note in hand".to_string(),
            },
        ];
        let dir = tempfile::tempdir().expect("tempdir");
        let (notes, inbox) = onejudge::note::Notes::channel();
        let (_ending, relayed) = relaying(dir.path(), notes);
        let answers = dispositions.clone();
        let conversation = std::thread::spawn(move || {
            for answer in answers {
                inbox
                    .take_within(Duration::from_secs(10))
                    .expect("a note reaches the conversation")
                    .answer(answer);
            }
        });
        for disposition in &dispositions {
            assert_eq!(
                submit(dir.path(), &note(Addressee::Both)).as_ref(),
                Ok(disposition),
                "{disposition:?} did not survive the spool"
            );
        }
        conversation.join().expect("the conversation");
        assert_eq!(relayed.taken(Duration::ZERO), dispositions.len());
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
