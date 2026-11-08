//! What restarts a clock: a `reset_on` filter, matched against every envelope
//! the run produces and every envelope appended to the stream it follows.
//!
//! A clock that declares `reset_on` is *subscribed* here when the run starts,
//! and reads its subscription on its own tick — the same tick it reads the
//! `trigger` and `reset` signal files on — so a match restarts it within one
//! tick without anything outside the run signalling it. Nothing here restarts
//! a clock that declared no `reset_on`: such a clock holds no subscription, so
//! no envelope, however plausible a match, reaches it.
//!
//! Two sources feed [`Resets::observe`]:
//!
//! * **The run's own envelopes**, through the observer the run's emitter is
//!   given ([`crate::event::Emitter`]): every one, before the output filter
//!   decides whether a consumer sees it — what a run acts on is unfiltered.
//!   Another clock's `cron-reset` is matched like any envelope; the one
//!   exclusion is a clock's own `cron-reset` about its own restart, which does
//!   not match that same clock, so a filter naming `cron-reset` cannot loop a
//!   clock on its own restart.
//! * **The followed stream** ([`follow`]): every line appended to the file after
//!   graph start, read as the stack's envelope with its `source` and `kind` kept
//!   as the wire strings they arrived as. A line that is not an envelope is
//!   skipped and counted, never fatal, and nothing read here is re-emitted.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::event::{Envelope, EventFilter, EventKind, ResetCause, SOURCE_WORD};
use crate::note::ScheduledNoteId;
use crate::run::MemberName;

/// How often a followed stream is read for what was appended to it — the
/// run's own clock tick, so a followed match reaches a clock no later than a
/// `reset-timer` would.
const FOLLOW_TICK: Duration = Duration::from_millis(100);

/// Every `reset_on` subscription of one run.
#[derive(Default)]
pub(crate) struct Resets {
    subscriptions: Mutex<Vec<Subscription>>,
}

/// One clock's filter, and the match it has not read yet.
struct Subscription {
    clock: ClockId,
    filter: EventFilter,
    /// The first match since the clock last read its subscription. One rather
    /// than a queue: a clock restarted twice inside one tick is restarted once.
    pending: Option<ResetCause>,
}

/// Which clock of a run a subscription belongs to: its member, and the
/// scheduled note when it is a note's clock rather than the member's own
/// `schedule`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClockId {
    member: MemberName,
    scheduled_note: Option<ScheduledNoteId>,
}

impl ClockId {
    /// The member's own `schedule`.
    pub(crate) fn schedule(member: MemberName) -> Self {
        Self {
            member,
            scheduled_note: None,
        }
    }

    /// The clock of `member`'s scheduled note `id`.
    pub(crate) fn note(member: MemberName, id: ScheduledNoteId) -> Self {
        Self {
            member,
            scheduled_note: Some(id),
        }
    }

    /// Whether `envelope` is this run's own `cron-reset` about this clock's
    /// restart. A followed one is another run's, whatever it calls itself.
    fn restarted_by(&self, envelope: &Envelope, from: Provenance) -> bool {
        from == Provenance::Own
            && envelope.kind.as_str() == EventKind::CronReset.as_str()
            && envelope.source.as_str() == SOURCE_WORD
            && envelope.labels.member.as_deref() == Some(self.member.as_str())
            && envelope
                .payload
                .get("scheduled_note")
                .and_then(serde_json::Value::as_str)
                == self.scheduled_note.as_ref().map(ScheduledNoteId::as_str)
    }
}

/// One clock's handle on its subscription.
#[derive(Clone)]
pub(crate) struct ResetOn {
    resets: Arc<Resets>,
    index: usize,
}

/// Where an envelope a subscription is matched against came from — which
/// decides whether it can be a reset's own echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Provenance {
    /// This run produced it.
    Own,
    /// It was appended to the stream this run follows.
    Followed,
}

impl Resets {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The handle a clock reads its matches through; its index is the clock's
    /// place in the list, which only grows.
    pub(crate) fn subscribe(self: &Arc<Self>, clock: ClockId, filter: EventFilter) -> ResetOn {
        let mut subscriptions = self.lock();
        subscriptions.push(Subscription {
            clock,
            filter,
            pending: None,
        });
        ResetOn {
            resets: Arc::clone(self),
            index: subscriptions.len() - 1,
        }
    }

    /// Match `envelope` against every subscription but that of the clock whose
    /// own restart it announces, so a filter naming `cron-reset` cannot loop a
    /// clock on its own restart. Any other clock's, this run's or a followed
    /// one, is matched like any envelope.
    pub(crate) fn observe(&self, envelope: &Envelope, from: Provenance) {
        let mut subscriptions = self.lock();
        for subscription in subscriptions.iter_mut() {
            if !subscription.clock.restarted_by(envelope, from)
                && subscription.pending.is_none()
                && subscription.filter.matches(envelope)
            {
                subscription.pending = Some(ResetCause::of(envelope));
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Subscription>> {
        self.subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl ResetOn {
    /// The match this clock has not read yet, taken.
    pub(crate) fn take(&self) -> Option<ResetCause> {
        self.resets
            .lock()
            .get_mut(self.index)
            .and_then(|subscription| subscription.pending.take())
    }
}

/// A followed stream being read, and how many lines in it were not envelopes.
pub(crate) struct Follower {
    stop: Arc<AtomicBool>,
    skipped: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Follower {
    /// Stop reading, and answer how many lines were skipped as not envelopes.
    pub(crate) fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.skipped.load(Ordering::SeqCst)
    }
}

/// Follow `path` from its end at this moment, feeding every envelope appended
/// to it to `resets`.
///
/// A path that does not exist yet is waited for rather than refused, and once it
/// appears everything in it was appended after this moment, so it is read from
/// its start. A file that shrinks was replaced, and is read again from its start.
///
/// # Errors
///
/// The host's refusal of the reading thread: a run asked to follow a stream
/// that it could not follow is refused, rather than run with every followed
/// reset silently missing.
pub(crate) fn follow(path: PathBuf, resets: Arc<Resets>) -> std::io::Result<Follower> {
    let stop = Arc::new(AtomicBool::new(false));
    let skipped = Arc::new(AtomicU64::new(0));
    // The end *now*, read on this thread rather than the follower's own, so an
    // envelope written before graph start is behind the offset however late the
    // thread is scheduled.
    let mut offset = std::fs::metadata(&path).map_or(0, |meta| meta.len());
    let thread = {
        let (stop, skipped) = (Arc::clone(&stop), Arc::clone(&skipped));
        std::thread::Builder::new()
            .name("follow-events".into())
            .spawn(move || {
                let mut partial = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    std::thread::sleep(FOLLOW_TICK);
                    let appended = read_from(&path, &mut offset, &mut partial);
                    for line in appended {
                        // A line that is not UTF-8 is not an envelope: it is
                        // counted rather than repaired into one whose words
                        // are not the ones its producer wrote.
                        let Ok(line) = String::from_utf8(line) else {
                            skipped.fetch_add(1, Ordering::SeqCst);
                            continue;
                        };
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Envelope>(line) {
                            Ok(envelope) => resets.observe(&envelope, Provenance::Followed),
                            Err(_) => {
                                skipped.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                    }
                }
            })?
    };
    Ok(Follower {
        stop,
        skipped,
        thread: Some(thread),
    })
}

/// The complete lines appended to `path` past `offset`, advancing it; a line
/// still being written stays in `partial` until its newline arrives.
fn read_from(path: &std::path::Path, offset: &mut u64, partial: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let length = file.metadata().map_or(0, |meta| meta.len());
    if length < *offset {
        *offset = 0;
        partial.clear();
    }
    if file.seek(SeekFrom::Start(*offset)).is_err() {
        return Vec::new();
    }
    let mut read = Vec::new();
    if file.read_to_end(&mut read).is_err() {
        return Vec::new();
    }
    *offset += read.len() as u64;
    partial.extend_from_slice(&read);
    let mut lines = Vec::new();
    while let Some(end) = partial.iter().position(|byte| *byte == b'\n') {
        let mut line: Vec<u8> = partial.drain(..=end).collect();
        line.pop();
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ScheduledNote;
    use crate::note::Addressee;

    fn schedule_of(member: &str) -> ClockId {
        ClockId::schedule(MemberName::parse(member).expect("a member"))
    }

    fn note_of(member: &str, id: &str) -> ClockId {
        let note = ScheduledNote {
            id: id.to_string(),
            every: 1,
            start_after: None,
            reset_on: None,
            addressee: Addressee::Worker,
            text: "t".to_string(),
        };
        ClockId::note(
            MemberName::parse(member).expect("a member"),
            ScheduledNoteId::of(&note),
        )
    }

    fn envelope(line: &str) -> Envelope {
        serde_json::from_str(line).expect("an envelope")
    }

    const SURFACED: &str = r#"{"v":1,"ts":"2026-11-08T00:00:00.000Z","stream":"pipeline-1","seq":7,"source":"pipeline","kind":"planner-surfaced","labels":{"run_id":"R"},"payload":{}}"#;

    #[test]
    fn a_foreign_envelope_matches_by_the_words_it_arrived_with_and_names_its_cause() {
        let resets = Resets::new();
        let filter: EventFilter =
            serde_json::from_str(r#"{"include":[{"source":"pipeline","kind":"planner-*"}]}"#)
                .expect("a filter");
        let clock = resets.subscribe(schedule_of("a"), filter);
        let other = resets.subscribe(
            schedule_of("b"),
            serde_json::from_str(r#"{"include":[{"kind":"member-died"}]}"#).expect("a filter"),
        );
        resets.observe(&envelope(SURFACED), Provenance::Followed);
        let cause = clock.take().expect("the match is pending");
        assert_eq!(
            (cause.source.as_str(), cause.kind.as_str(), cause.seq),
            ("pipeline", "planner-surfaced", 7)
        );
        assert_eq!(cause.stream, "pipeline-1");
        assert!(clock.take().is_none(), "a match is read once");
        assert!(
            other.take().is_none(),
            "a filter that did not match is untouched"
        );
    }

    #[test]
    fn a_clock_is_restarted_by_every_cron_reset_but_its_own() {
        let resets = Resets::new();
        let any_reset = || serde_json::from_str(r#"{"include":[{"kind":"cron-*"}]}"#).unwrap();
        let schedule = resets.subscribe(schedule_of("m"), any_reset());
        let note = resets.subscribe(note_of("m", "n"), any_reset());
        let reset = SURFACED
            .replace(
                "\"pipeline\",\"kind\":\"planner-surfaced\"",
                "\"agentgraph\",\"kind\":\"cron-reset\"",
            )
            .replace("\"run_id\":\"R\"", "\"member\":\"m\"");
        // The member's schedule restarted: its note's clock hears, it does not.
        resets.observe(&envelope(&reset), Provenance::Own);
        assert!(schedule.take().is_none());
        assert!(note.take().is_some());
        // The same words on a followed stream are another run's.
        resets.observe(&envelope(&reset), Provenance::Followed);
        assert!(schedule.take().is_some());
        assert!(note.take().is_some());
        // The note's clock restarted: the schedule hears, it does not.
        let of_note = reset.replace("\"payload\":{}", "\"payload\":{\"scheduled_note\":\"n\"}");
        resets.observe(&envelope(&of_note), Provenance::Own);
        assert!(note.take().is_none());
        assert!(schedule.take().is_some());
    }

    #[test]
    fn a_followed_file_is_read_from_its_end_at_the_start_and_skips_what_is_not_an_envelope() {
        let dir = tempfile::tempdir().expect("a dir");
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, format!("{SURFACED}\n")).expect("written before the start");
        let resets = Resets::new();
        let clock = resets.subscribe(
            schedule_of("m"),
            serde_json::from_str(r#"{"include":[{"kind":"planner-surfaced"}]}"#).expect("a filter"),
        );
        let follower = follow(path.clone(), Arc::clone(&resets)).expect("a follower");
        std::thread::sleep(FOLLOW_TICK * 3);
        assert!(
            clock.take().is_none(),
            "an envelope written before the start matched"
        );
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("appendable");
        std::io::Write::write_all(&mut file, b"not an envelope\n").expect("append");
        std::io::Write::write_all(&mut file, b"\xff\xfe{}\n").expect("append");
        std::io::Write::write_all(&mut file, SURFACED.as_bytes()).expect("append");
        std::thread::sleep(FOLLOW_TICK * 3);
        assert!(
            clock.take().is_none(),
            "a line with no newline yet was read"
        );
        std::io::Write::write_all(&mut file, b"\n").expect("append");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut cause = None;
        while cause.is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(FOLLOW_TICK);
            cause = clock.take();
        }
        assert!(cause.is_some(), "an appended envelope never matched");
        assert_eq!(follower.finish(), 2, "a malformed line was not counted");
    }

    #[test]
    fn a_followed_path_that_does_not_exist_yet_is_waited_for() {
        let dir = tempfile::tempdir().expect("a dir");
        let path = dir.path().join("later.jsonl");
        let resets = Resets::new();
        let clock = resets.subscribe(
            schedule_of("m"),
            serde_json::from_str(r#"{"include":[{"source":"pipeline"}]}"#).expect("a filter"),
        );
        let follower = follow(path.clone(), Arc::clone(&resets)).expect("a follower");
        std::thread::sleep(FOLLOW_TICK * 2);
        std::fs::write(&path, format!("{SURFACED}\n")).expect("created after the start");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut cause = None;
        while cause.is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(FOLLOW_TICK);
            cause = clock.take();
        }
        assert!(
            cause.is_some(),
            "a file created after the start was never read"
        );
        assert_eq!(follower.finish(), 0);
    }
}
