//! Running a member's declared pre-turn commands, and folding what they printed
//! into the instruction that turn receives.
//!
//! A supervisory member opens each turn by *going and looking*: spending tool
//! calls to rediscover state that already exists and is already labelled, and
//! then reporting on whichever half of it the turn got to. Three false alarms in
//! one session came from exactly that — a command called unable to advance was
//! halfway through its own bound and finished on its own; a report of no matching
//! processes was wrong because the output was going to a log. This is the other
//! shape of the same member: it is **handed** the state, its first act is reading
//! a prepared view, and it investigates only what looks strange.
//!
//! # What is deliberately not decided here
//!
//! Not the task, not the persona, and not when a turn happens. A view adds
//! context to a turn that was going to be taken anyway, and *which* view a member
//! reads is the consuming host's decision — this crate ships the capability, not
//! a wiring of it.
//!
//! # The four ways a view can fail, and why none of them fails the member
//!
//! The turn is the valuable thing and the context is an aid to it, so every one
//! of these degrades to the turn happening without that view — and every one of
//! them is *said*, in the turn's own context and on the run's event stream, never
//! swallowed:
//!
//! * **It could not be started.** [`PreTurnOutcome::Unspawnable`].
//! * **It exited non-zero**, or a signal ended it. [`PreTurnOutcome::Failed`],
//!   carrying the status and the tail of standard error.
//! * **It printed nothing.** [`PreTurnOutcome::Empty`] — a distinct fact from a
//!   view that says the queue is empty, and a supervisor branches on the
//!   difference.
//! * **It did not finish.** [`PreTurnOutcome::TimedOut`], at the view's **own**
//!   bound. That bound is the load-bearing one: a single-sided member has no
//!   per-turn deadline at all, so a view without one of its own could wedge the
//!   member forever, and `crate::member`'s activity watchdog cannot help — it
//!   does not start until the engine does.
//!
//! # Bounds
//!
//! Command output spliced into a model's context is a cost, so what is injected
//! is cut at [`MAX_PRE_TURN_OUTPUT_BYTES`] per view and the cut is **marked in
//! the context itself**, not only on the stream: a view that reads as complete
//! and is not is the same defect as a supervisor reporting on state it never saw.
//! The whole output is not kept anywhere by this module — the bound is applied as
//! the pipe is drained, so a runaway view costs the run its own bound in time and
//! nothing in memory.
//!
//! # Which stream is the view
//!
//! Standard output, and only that. Standard error is captured too, but it is
//! *evidence about a failure* rather than context — it is what a `failed` view's
//! `detail` carries, and it never reaches the model. Mixing the two would put a
//! progress bar in a supervisor's context and call it state.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::PreTurn;
use crate::event::{bound_text, Emitter, EventKind, PreTurnContext, PreTurnOutcome};
use crate::member::as_payload;
use crate::scratch::Group;

/// The most of one view's output that reaches a turn's context.
///
/// Sixteen kilobytes: a prepared view — a status table, a queue, a timeline —
/// fits in it several times over, and a command that has outgrown it has stopped
/// being a view and become a document, which a member should be *told where to
/// find* rather than handed. Per view, and [`crate::config::MAX_PRE_TURN_COMMANDS`]
/// bounds how many views there are, so the total a turn can carry is bounded too.
pub const MAX_PRE_TURN_OUTPUT_BYTES: usize = 16 * 1024;

/// How often a running view is asked whether it has finished.
///
/// Short relative to the shortest bound anyone can name (one second), so a view
/// that finishes is not held by the polling, and long enough that waiting out a
/// five-minute one is not a spin.
const POLL: Duration = Duration::from_millis(20);

/// One view, prepared: a command [`crate::config::validate`] has already accepted,
/// with the two things it named resolved.
///
/// Built where every other launch decision is built ([`crate::invoke`]) and
/// carried on the launch, rather than re-derived per turn: a scheduled member
/// runs this list every firing, and a value re-decided per turn is one that can
/// answer differently the second time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// What this view is called, in the turn's context and on the stream.
    pub label: String,
    /// The argv, program first.
    pub command: Vec<String>,
    /// How long this command may run before the turn goes on without it.
    pub timeout: Duration,
}

impl View {
    /// The view one declaration describes.
    #[must_use]
    pub fn declared(declared: &PreTurn) -> Self {
        Self {
            label: declared.view().to_string(),
            command: declared.command.clone(),
            timeout: Duration::from_secs(declared.seconds()),
        }
    }
}

/// The instruction a turn receives: every declared view's output, then the
/// member's own prose.
///
/// A member that declared no view is returned its prose **unchanged and untouched
/// by any of this** — no process is started, nothing is rendered, and the turn is
/// the turn it always was.
///
/// `dir` is the directory the member works in, which is where a view runs: a
/// member asking what its worktree looks like means *its* worktree.
pub(crate) fn instruction(
    views: &[View],
    dir: &Path,
    group: &Group,
    scratch: &Path,
    emitter: &Emitter,
    prompt: &str,
) -> String {
    if views.is_empty() {
        return prompt.to_string();
    }
    let gathered: Vec<(&View, Captured)> = views
        .iter()
        .map(|view| {
            let captured = capture(view, dir, group, scratch);
            emitter.emit(
                EventKind::PreTurnContext,
                as_payload(&published(view, &captured)),
            );
            (view, captured)
        })
        .collect();
    format!("{}\n\n{prompt}", rendered(&gathered))
}

/// What one view's `pre-turn-context` says on the wire.
fn published(view: &View, captured: &Captured) -> PreTurnContext {
    PreTurnContext {
        label: view.label.clone(),
        command: view.command.clone(),
        outcome: captured.outcome,
        bytes: captured.text.len() as u64,
        truncated: captured.truncated,
        detail: captured
            .detail
            .as_deref()
            .map(|detail| bound_text(detail).0),
    }
}

/// The context block a turn is opened with, as the model meets it.
///
/// Tagged rather than prose, and every view named whatever became of it: a
/// supervisor reading this has to be able to tell "the queue is empty" from
/// "there is no queue view", and a block that quietly omitted the failed one
/// reads as the first while meaning the second.
fn rendered(gathered: &[(&View, Captured)]) -> String {
    let mut block = String::from("<pre-turn-context>\n");
    for (view, captured) in gathered {
        block.push_str(&format!("<view name=\"{}\"", escaped(&view.label)));
        if captured.truncated {
            block.push_str(&format!(
                " truncated=\"kept the first {MAX_PRE_TURN_OUTPUT_BYTES} bytes\""
            ));
        }
        if captured.outcome == PreTurnOutcome::Captured {
            block.push_str(&format!(
                ">\n{}\n</view>\n",
                captured.text.trim_end_matches('\n')
            ));
        } else {
            // No content, and the reason in its place — an empty element rather
            // than an empty body, so a view that did not run never reads as one
            // that ran and had nothing to say.
            block.push_str(&format!(
                " unavailable=\"{}\" />\n",
                escaped(captured.detail.as_deref().unwrap_or("no output"))
            ));
        }
    }
    block.push_str("</pre-turn-context>");
    block
}

/// One attribute value, with the four characters that would forge the framing
/// around it spelled as entities.
///
/// A label comes from the graph document and a detail from a program's standard
/// error, and neither is this crate's to trust with the shape of the block it
/// sits in. Newlines go too: an attribute is one line.
fn escaped(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\n' | '\r' => " ".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// What running one view produced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Captured {
    /// Which of the five things happened.
    outcome: PreTurnOutcome,
    /// The output that reaches the turn — empty for every outcome but
    /// [`PreTurnOutcome::Captured`].
    text: String,
    /// Whether that output was cut at [`MAX_PRE_TURN_OUTPUT_BYTES`].
    truncated: bool,
    /// Why there is no output, for a view that produced none.
    detail: Option<String>,
}

impl Captured {
    /// A view that contributed nothing, and what it says instead.
    fn nothing(outcome: PreTurnOutcome, detail: String) -> Self {
        Self {
            outcome,
            text: String::new(),
            truncated: false,
            detail: Some(detail),
        }
    }
}

/// Run one view to its end, its own bound, or the failure that stopped it.
///
/// Spawned through the member's own [`Group`], so a view that leaves a child
/// behind leaves it somewhere a cancel, a sweep, and the reap below can still
/// reach — the same containment every other process this crate starts is under.
fn capture(view: &View, dir: &Path, group: &Group, scratch: &Path) -> Captured {
    let (program, arguments) = match view.command.split_first() {
        Some(split) => split,
        // `validate` refuses an empty command, so a view reaching here has a
        // program. Answered rather than asserted, because this module's whole
        // contract is that nothing about a view can fail a member.
        None => {
            return Captured::nothing(
                PreTurnOutcome::Unspawnable,
                "this view names no command to run".to_string(),
            )
        }
    };
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(dir)
        // Null, never inherited: a view that read standard input would race the
        // run for whatever the operator is typing, and block forever when there
        // is nobody there — which is a wedge its own bound would then have to
        // clean up.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match group.spawn(&mut command) {
        Ok(child) => child,
        Err(err) => {
            return Captured::nothing(
                PreTurnOutcome::Unspawnable,
                format!("cannot run {program}: {err}"),
            )
        }
    };

    // Drained on threads of their own, and started before the wait: a view that
    // fills a pipe nobody is reading blocks in the kernel, and would then be
    // stopped by its own bound having done its work perfectly well.
    let out = drain(child.stdout.take(), MAX_PRE_TURN_OUTPUT_BYTES);
    let err = drain(child.stderr.take(), MAX_PRE_TURN_OUTPUT_BYTES);
    let (out, err) = match (out, err) {
        (Ok(out), Ok(err)) => (out, err),
        // llmlint: ignore-block[changed_behavior_has_e2e] no graph, task, or
        // config reaches this arm: it is taken only when the OS refuses a thread,
        // which is a host resource limit, and no seam this crate sanctions fakes
        // `pthread_create` — `crate::harness` records the same reasoning for the
        // engine thread it spawns. What it decides is the direction of an
        // unreachable failure, and it is the safe one: the child is stopped and
        // the turn goes on without this view.
        (out, err) => {
            let _ = child.kill();
            let _ = child.wait();
            let refusal = out.err().or_else(|| err.err());
            return Captured::nothing(
                PreTurnOutcome::Unspawnable,
                format!(
                    "cannot read what {program} printed: {}",
                    refusal.map_or_else(|| "no reader".to_string(), |err| err.to_string())
                ),
            );
        } // llmlint: ignore-end[changed_behavior_has_e2e]
    };

    let deadline = Instant::now() + view.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => break None,
            Ok(None) => std::thread::sleep(POLL),
            // llmlint: ignore-block[changed_behavior_has_e2e] `try_wait` fails
            // only when the kernel refuses to report on a child this process
            // itself started, which no input reaches; the arm exists so a view
            // that cannot be waited on ends the wait rather than spinning until
            // its bound. Covered by `a_view_that_never_finishes_is_stopped_at_its
            // _own_bound` for the shape it shares — the child is stopped and the
            // turn goes on.
            Err(_) => break None,
            // llmlint: ignore-end[changed_behavior_has_e2e]
        }
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        // The kill reaches this child; the reap reaches whatever it started,
        // which is the half a `kill` on a pid cannot see. Both, because a view
        // that spawns and hangs is exactly the shape that leaves a tree behind.
        let stray = crate::scratch::reap(scratch);
        return Captured::nothing(
            PreTurnOutcome::TimedOut,
            format!(
                "{program} did not finish inside {}s, so the turn went on without it ({stray} \
                 process(es) were stopped with it)",
                view.timeout.as_secs()
            ),
        );
    };
    let (text, truncated) = out.join().unwrap_or_default();
    let (stderr, _) = err.join().unwrap_or_default();
    if !status.success() {
        return Captured::nothing(
            PreTurnOutcome::Failed,
            format!(
                "{program} {}{}",
                status.code().map_or_else(
                    || "was ended by a signal".to_string(),
                    |code| format!("exited {code}")
                ),
                tail(&stderr),
            ),
        );
    }
    if text.trim().is_empty() {
        return Captured::nothing(
            PreTurnOutcome::Empty,
            format!("{program} exited 0 having printed nothing"),
        );
    }
    Captured {
        outcome: PreTurnOutcome::Captured,
        text,
        truncated,
        detail: None,
    }
}

/// What a failing view said on standard error, as a clause of the reason.
///
/// The last line of it, the way `crate::render` renders a death's detail: what
/// names a failure is the end of what the program printed, and the whole of it
/// would put a view's log into an event payload.
fn tail(stderr: &str) -> String {
    match stderr.trim().lines().next_back() {
        Some(last) if !last.trim().is_empty() => format!(": {}", last.trim()),
        _ => String::new(),
    }
}

/// Read one of a view's pipes to its end, keeping at most `keep` bytes of it.
///
/// Keeps the **head**, unlike every other bound in this crate: a prepared view is
/// written opening-first — what it is, then the rows — so the front of it is the
/// part a member was told to read. The rest is still consumed rather than left in
/// the pipe, which is what keeps a chatty view from blocking on a reader that
/// stopped listening.
///
/// The pipe is an `Option` because [`std::process::Child`] hands it over that way;
/// `None` reads as nothing rather than as a failure, so no caller has an
/// unreachable arm to answer for.
fn drain(
    pipe: Option<impl Read + Send + 'static>,
    keep: usize,
) -> std::io::Result<std::thread::JoinHandle<(String, bool)>> {
    std::thread::Builder::new().spawn(move || {
        let mut source: Box<dyn Read> = match pipe {
            Some(pipe) => Box::new(pipe),
            None => Box::new(std::io::empty()),
        };
        let mut kept: Vec<u8> = Vec::new();
        let mut buffer = [0u8; 8192];
        let mut truncated = false;
        loop {
            match source.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let room = keep.saturating_sub(kept.len());
                    if read > room {
                        truncated = true;
                    }
                    kept.extend_from_slice(&buffer[..read.min(room)]);
                }
            }
        }
        // On a character boundary, so a cut never lands inside a multi-byte
        // character and turns a view into replacement marks.
        let mut end = kept.len();
        while end > 0 && std::str::from_utf8(&kept[..end]).is_err() {
            end -= 1;
        }
        (
            String::from_utf8_lossy(&kept[..end]).into_owned(),
            truncated,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(label: &str) -> View {
        View {
            label: label.to_string(),
            command: vec!["queue-depth".to_string()],
            timeout: Duration::from_secs(1),
        }
    }

    fn captured(text: &str, truncated: bool) -> Captured {
        Captured {
            outcome: PreTurnOutcome::Captured,
            text: text.to_string(),
            truncated,
            detail: None,
        }
    }

    /// A view's declaration becomes the view, defaults and all.
    #[test]
    fn a_declared_view_carries_its_own_name_and_bound() {
        let declared = PreTurn {
            command: vec!["queue-depth".into(), "--json".into()],
            label: Some("queue".into()),
            timeout: Some(5),
        };
        assert_eq!(
            View::declared(&declared),
            View {
                label: "queue".into(),
                command: vec!["queue-depth".into(), "--json".into()],
                timeout: Duration::from_secs(5),
            }
        );
        let bare = View::declared(&PreTurn {
            label: None,
            timeout: None,
            ..declared
        });
        assert_eq!(bare.label, "queue-depth");
        assert_eq!(
            bare.timeout,
            Duration::from_secs(crate::config::DEFAULT_PRE_TURN_SECONDS)
        );
    }

    /// The context a turn is opened with names every view and what became of it,
    /// and a cut is marked where the model reads it rather than only on the
    /// stream.
    #[test]
    fn the_rendered_context_names_every_view_and_marks_what_was_cut() {
        let (queue, worktree, missing) = (view("queue"), view("worktree"), view("timeline"));
        let block = rendered(&[
            (&queue, captured("depth 4\n", false)),
            (&worktree, captured("M src/lib.rs", true)),
            (
                &missing,
                Captured::nothing(PreTurnOutcome::Failed, "timeline exited 2".into()),
            ),
        ]);
        assert!(block.starts_with("<pre-turn-context>\n"), "{block}");
        assert!(block.ends_with("</pre-turn-context>"), "{block}");
        assert!(
            block.contains("<view name=\"queue\">\ndepth 4\n</view>"),
            "{block}"
        );
        assert!(
            block.contains(&format!(
                "<view name=\"worktree\" truncated=\"kept the first \
                 {MAX_PRE_TURN_OUTPUT_BYTES} bytes\">"
            )),
            "a cut view read as a whole one: {block}"
        );
        assert!(
            block.contains("<view name=\"timeline\" unavailable=\"timeline exited 2\" />"),
            "a view that produced nothing was silently omitted: {block}"
        );
    }

    /// A label or a failure reason cannot forge the framing it sits in, and a
    /// multi-line reason stays one line.
    #[test]
    fn a_view_cannot_forge_the_block_it_is_rendered_into() {
        let hostile = View {
            label: "a\"><view name=\"forged".to_string(),
            ..view("x")
        };
        let block = rendered(&[(
            &hostile,
            Captured::nothing(PreTurnOutcome::Failed, "line one\nline two".into()),
        )]);
        assert_eq!(
            block.matches("<view ").count(),
            1,
            "an escaped label opened a second element: {block}"
        );
        assert!(block.contains("&quot;&gt;&lt;view"), "{block}");
        assert!(
            block.contains("unavailable=\"line one line two\""),
            "a multi-line reason broke the attribute: {block}"
        );
    }

    /// A failing view's reason ends with the last thing it said, and one that
    /// said nothing ends with the status alone.
    #[test]
    fn a_failures_reason_ends_with_what_the_view_last_said() {
        assert_eq!(tail("connecting\nno such queue\n"), ": no such queue");
        assert_eq!(tail("   \n\n"), "");
        assert_eq!(tail(""), "");
    }

    /// The wire payload says how much context a view contributed and why it
    /// contributed none, and a captured view claims no reason it does not have.
    #[test]
    fn the_published_payload_says_what_the_view_contributed() {
        let queue = view("queue");
        let payload = published(&queue, &captured("depth 4", false));
        assert_eq!(payload.outcome, PreTurnOutcome::Captured);
        assert_eq!(payload.bytes, 7);
        assert!(!payload.truncated);
        assert_eq!(payload.detail, None);
        assert_eq!(payload.command, queue.command);

        let empty = published(
            &queue,
            &Captured::nothing(PreTurnOutcome::Empty, "queue-depth printed nothing".into()),
        );
        assert_eq!(empty.outcome, PreTurnOutcome::Empty);
        assert_eq!(empty.bytes, 0);
        assert_eq!(empty.detail.as_deref(), Some("queue-depth printed nothing"));

        // A reason as long as a program cares to print is bounded like every
        // other payload text field.
        let long = published(
            &queue,
            &Captured::nothing(
                PreTurnOutcome::Failed,
                "x".repeat(crate::event::MAX_PAYLOAD_TEXT_BYTES * 2),
            ),
        );
        assert_eq!(
            long.detail.map(|detail| detail.len()),
            Some(crate::event::MAX_PAYLOAD_TEXT_BYTES)
        );
    }

    /// Every outcome has one spelling on the wire, and it round-trips.
    #[test]
    fn every_outcome_has_one_spelling_that_round_trips() {
        for outcome in [
            PreTurnOutcome::Captured,
            PreTurnOutcome::Empty,
            PreTurnOutcome::Failed,
            PreTurnOutcome::Unspawnable,
            PreTurnOutcome::TimedOut,
        ] {
            let wire = serde_json::to_value(outcome).expect("serializes");
            assert_eq!(wire, serde_json::Value::from(outcome.as_str()));
            assert_eq!(
                serde_json::from_value::<PreTurnOutcome>(wire).expect("reads back"),
                outcome
            );
        }
    }

    /// A pipe longer than the bound keeps its **opening**, says it was cut, and
    /// is drained to the end regardless — and a cut lands on a character
    /// boundary rather than inside one.
    #[test]
    fn a_view_longer_than_the_bound_keeps_its_opening_and_says_so() {
        let long = format!("depth 4\n{}", "é".repeat(MAX_PRE_TURN_OUTPUT_BYTES));
        let (text, truncated) = drain(
            Some(std::io::Cursor::new(long.into_bytes())),
            MAX_PRE_TURN_OUTPUT_BYTES,
        )
        .expect("a reader")
        .join()
        .expect("the reader finished");
        assert!(text.starts_with("depth 4\n"), "{text}");
        assert!(truncated);
        assert!(text.len() <= MAX_PRE_TURN_OUTPUT_BYTES);
        assert!(
            !text.contains('\u{fffd}'),
            "the cut landed inside a character"
        );

        // Inside the bound: served whole, claiming no cut.
        let (whole, cut) = drain(Some(std::io::Cursor::new(b"depth 4".to_vec())), 64)
            .expect("a reader")
            .join()
            .expect("the reader finished");
        assert_eq!(whole, "depth 4");
        assert!(!cut);

        // A pipe the child never gave us reads as nothing rather than failing.
        let (nothing, cut) = drain(None::<std::io::Cursor<Vec<u8>>>, 64)
            .expect("a reader")
            .join()
            .expect("the reader finished");
        assert_eq!(nothing, "");
        assert!(!cut);
    }
}
