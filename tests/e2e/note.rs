//! Role-addressed notes: an update to one party's task, routed to whichever side
//! of a two-party member is live.
//!
//! `interrupt` aims one redirection at the **agent** side's socket, which is why
//! a note delivered that way has only ever reached the worker: the judge has no
//! socket, so a manager's ruling reached the party doing the work and never the
//! party judging it — and that judge went on reviewing against a task the ruling
//! had changed. These journeys are the routed path beside it: the note is offered
//! to the member itself, the member decides from the conversation it is actually
//! having, and the addressed role reaches the receiving party unchanged.
//!
//! Every journey here is driven through the **library**, because that is what the
//! seam is required on: a consumer of this crate calls
//! [`oneagentgraph::control::note`] and never types a verb. The `interrupt` verb
//! is untouched and its own journeys are in `tests/e2e/interrupt.rs`.

// llmlint: ignore-file[e2e_not_mocked] the same declaration its sibling journey
// files carry, and for the same reason — see tests/e2e/support.rs: the paid
// harness process is the one sanctioned double, replaced at oneharness's own
// `ONEHARNESS_BIN_<ID>` seam. Real `oneagentgraph` routes, real `onejudge` runs
// the conversation, real `oneharness` opens the control socket and serves the
// delivery over claude-code's own stdin control protocol.

// The routed delivery reaches a turn through a unix domain socket, so the
// journeys that drive one are `cfg(unix)` exactly as the interrupt journeys are.
// The fixtures below are still compiled on Windows — type-checked and linted —
// with nothing calling them.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use oneagentgraph::config::ConfigRef;
use oneagentgraph::control::{self, Accepted, Addressee, Note, NoteDelivery, Undelivered};
use oneagentgraph::run::{self, MemberName, Request};

use crate::support::{oneharness_bin, two_party_graph, until, Workspace, BASE};

/// Journeys here start real runs with real session stores; one at a time, as the
/// library journeys next door are, so two runs never race for the same host.
static NOTE_RUN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One started run and everything a journey addresses it by.
struct Running {
    /// The run, still going.
    run: run::Running,
    /// What a note names it.
    id: run::RunId,
    /// The member every journey here addresses.
    member: MemberName,
}

/// Start the two-party graph in `workspace` on `task`, with `system` appended to
/// the agent's standing system prompt.
///
/// The system prompt is the one text every **agent** turn of a conversation
/// carries — a task steers only the turn it opens, and a supervisor's message
/// steers only the one it opens — so a journey that needs the same behaviour on
/// the turn *after* a redirection puts it here.
fn start(workspace: &Workspace, system: &str, task: &str) -> Running {
    workspace.write("base.yaml", &base_with(system));
    workspace.graph(&two_party_graph(
        &crate::support::fake_harness(),
        &[(
            "XDG_STATE_HOME",
            workspace.session_store().display().to_string(),
        )],
    ));
    let mut env = BTreeMap::new();
    env.insert(
        "XDG_STATE_HOME".to_string(),
        workspace.session_store().display().to_string(),
    );
    let request = Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: Some(task.to_string()),
        dir: workspace.dir(),
        labels: Vec::new(),
        overrides: Vec::new(),
        filter: None,
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };
    let run = run::start(&request, &env).expect("the graph starts");
    let id = run.started().run_id.clone();
    Running {
        run,
        id,
        member: MemberName::parse("worker").expect("a member name"),
    }
}

/// The default onejudge base with `system` added to the standing system prompt.
///
/// Parsed and reserialized rather than formatted in, for the reason
/// [`crate::support::graph_with`] records: `system_prompt` is a block scalar, and
/// text appended to the rendered document lands under whatever key happens to
/// follow it.
fn base_with(system: &str) -> String {
    let mut document: serde_norway::Value =
        serde_norway::from_str(BASE).expect("the base is a YAML document");
    let key = serde_norway::Value::String("system_prompt".to_string());
    let mapping = document.as_mapping_mut().expect("the base is a mapping");
    let standing = mapping
        .get(&key)
        .and_then(serde_norway::Value::as_str)
        .unwrap_or_default()
        .to_string();
    mapping.insert(
        key,
        serde_norway::Value::String(format!("{standing}\n{system}\n")),
    );
    serde_norway::to_string(&document).expect("the base serializes")
}

/// Offer one note to a live member through the library.
fn offer(workspace: &Workspace, running: &Running, note: &Note) -> NoteDelivery {
    control::note(
        &workspace.state(),
        &running.id,
        &running.member,
        note,
        &oneharness_bin(),
    )
    .expect("the run and its member are addressable")
}

/// A note delivered into the worker's **live** turn: it stops what that turn was
/// doing and does the update instead, and the addressed role arrives with it.
///
/// The whole assertion is the two files. `did-original-work` is appended by a
/// controlled turn once it is past its park and past any abort, so its absence
/// proves the parked turn never got there; `did-work` is what the *redirected*
/// turn appends, and it is that turn's own prompt — so what it contains is
/// exactly what the worker was handed. The role is in it because a note is
/// addressed: a party that cannot tell whose task an update belongs to reads
/// every one of them as its own next instruction.
#[cfg(unix)]
#[test]
fn a_note_during_a_live_worker_turn_reaches_the_worker_carrying_its_role() {
    let _serial = NOTE_RUN.lock().expect("note journey lock");
    let workspace = Workspace::new();
    let started = workspace.at("turn-started");
    let original_work = workspace.at("did-original-work");
    let redirected_work = workspace.at("did-redirected-work");
    let running = start(
        &workspace,
        &format!("fake:did-work={}", redirected_work.display()),
        &format!(
            "fake:park fake:started={} fake:did-work={}",
            started.display(),
            original_work.display()
        ),
    );

    // Only a *controlled* turn writes this, which makes the wait a check on the
    // control ask as well as on the turn: a timeout here means oneharness refused
    // `--control` and the turn ran with no lever at all.
    until(
        "the worker's turn to be in flight — only a controlled turn writes this",
        || started.exists(),
    );

    let text = "fake:complete-now the release blocker is P0: fix it before anything else";
    let note = Note::new(Addressee::Worker, text).expect("a note with text in it");
    let delivery = offer(&workspace, &running, &note);
    assert_eq!(
        delivery,
        NoteDelivery::Accepted(Accepted::Interrupted),
        "a note offered while the worker's turn was live did not go into it"
    );

    assert_eq!(
        running.run.wait().expect("the member settles"),
        0,
        "the member did not settle after the note"
    );
    assert!(
        !original_work.exists(),
        "the turn the note reached went on and did the work it was parked before — it was added \
         to rather than redirected"
    );
    let handed = std::fs::read_to_string(&redirected_work)
        .expect("the turn that ran after the note never recorded what it was handed");
    assert!(
        handed.contains(text),
        "the worker was not handed the note's own text: {handed:?}"
    );
    assert!(
        handed.contains("addressed to: worker"),
        "the note reached the worker without saying whose task it updates: {handed:?}"
    );
}

/// A note offered while the **judge's** turn is live is queued rather than
/// forced, and reaches the worker with the judge's own response.
///
/// This is the case `interrupt` cannot express at all. onejudge opens a
/// controllable turn for the agent side alone, so there is nothing to redirect
/// while the supervisor is deciding — an `interrupt` here is refused as *between
/// turns*, and a caller that wanted the update delivered has to sit and retry.
/// The routed path reads the live side off the conversation instead: the note is
/// held, and the very next worker turn — the one the judge's response opens — is
/// where it lands.
///
/// The fixture is three of the double's own sentinels and nothing else: the
/// supervisor's turn holds where a journey can see it and release it, the turn it
/// then asks for is one this journey spelled out, and that turn waits — so what
/// the note reaches is a worker turn that is really in flight rather than one it
/// raced.
#[cfg(unix)]
#[test]
fn a_note_during_a_live_judge_turn_is_queued_and_reaches_the_worker_with_the_response() {
    let _serial = NOTE_RUN.lock().expect("note journey lock");
    let workspace = Workspace::new();
    let judging = workspace.at("judge-gate");
    let judge_entered = workspace.at("judge-gate.entered");
    let handed_over = workspace.at("handed-over");
    // The instruction the supervisor asks for once, and which the worker's next
    // turn parks on — so the only thing that ends it is the note's own delivery.
    //
    // `park` rather than `hold`, and that is not interchangeable here: a judge
    // side's prompt embeds the transcript, so a `hold` this turn was given comes
    // back to the supervisor on its *next* call and stalls the whole run on a
    // gate the journey has no reason to open. `park` answers on a controlled turn
    // alone, which the judge's never is.
    let next = workspace.write("next-turn", "fake:park");
    let running = start(
        &workspace,
        &format!("fake:did-work={}", handed_over.display()),
        &format!(
            "fake:supervisor-hold={} fake:next-turn={}",
            judging.display(),
            next.display()
        ),
    );

    until("the judge's own turn to be in flight", || {
        judge_entered.exists()
    });

    let text = "the acceptance bar moved: the migration has to be reversible";
    let note = Note::new(Addressee::Worker, text)
        .expect("a note with text in it")
        .binding();
    let delivery = offer(&workspace, &running, &note);
    assert_eq!(
        delivery,
        NoteDelivery::Accepted(Accepted::Queued),
        "a note offered while the judge was deciding was not queued for the turn after it"
    );

    // The judge answers, and the turn it asks for is the one the note is waiting
    // for.
    std::fs::write(&judging, "go").expect("release the judge");

    assert_eq!(
        running.run.wait().expect("the member settles"),
        0,
        "the member did not settle after the queued note"
    );
    let handed =
        std::fs::read_to_string(&handed_over).expect("no worker turn recorded what it was handed");
    assert!(
        handed.contains(text),
        "the queued note never reached the worker: {handed:?}"
    );
    assert!(
        handed.contains("addressed to: worker"),
        "the queued note reached the worker without saying whose task it updates: {handed:?}"
    );
    assert!(
        handed.contains("judged against"),
        "a note that amends the task did not say so to the party judged against it: {handed:?}"
    );
}

/// A note that cannot be delivered **raises**, and says which of the two reasons
/// applies.
///
/// This is the half that is an error rather than an answer, and it is the failure
/// the whole seam exists to remove: a note accepted into a member nothing would
/// ever read it out of looks, to the caller, exactly like one that landed. Both
/// refusals are driven here — a member the run has settled, and a member whose
/// conversation reached its completion decision — and neither is silently taken.
#[test]
fn a_note_that_cannot_be_delivered_says_so_rather_than_being_accepted() {
    let _serial = NOTE_RUN.lock().expect("note journey lock");
    let workspace = Workspace::new();
    let running = start(&workspace, "", "fake:complete-now: a member that finishes");
    let Running { run, id, member } = running;
    assert_eq!(
        run.wait().expect("the member settles"),
        0,
        "the member did not settle"
    );

    let note = Note::new(Addressee::Worker, "one more thing").expect("a note with text in it");
    let settled = control::note(&workspace.state(), &id, &member, &note, &oneharness_bin())
        .expect("the run and its member are addressable");
    assert!(
        matches!(
            &settled,
            NoteDelivery::Undelivered(Undelivered::MemberSettled { outcome })
                if outcome.contains("Settled")
        ),
        "a note to a settled member was not refused as undelivered: {settled:?}"
    );
    let NoteDelivery::Undelivered(undelivered) = &settled else {
        unreachable!("the match above")
    };
    assert!(
        undelivered.to_string().contains("was not delivered"),
        "the refusal did not say the note was not delivered: {undelivered}"
    );

    // The other refusal, reached the way it happens in a run: the member's own
    // thread records the completion its conversation ended on, and a note offered
    // after that is answered by the conversation that could not take it — which
    // is the window the old path swallowed, because the run's record still says
    // the member is going.
    let scratch = workspace
        .state()
        .join(id.as_str())
        .join("members")
        .join("worker");
    let completed = std::fs::read_to_string(scratch.join("notes").join("completed.json"))
        .expect("the member recorded what its conversation ended on");
    assert!(
        completed.contains("completion_reason"),
        "the completion record named no reason: {completed}"
    );
    let refused = oneagentgraph::note::submit(&scratch, &note)
        .expect_err("a completed conversation cannot take a note");
    assert!(
        matches!(&refused, Undelivered::ConversationCompleted { completion_reason }
            if !completion_reason.is_empty()),
        "{refused:?}"
    );
    assert!(
        refused.to_string().contains("was not delivered"),
        "the refusal did not say the note was not delivered: {refused}"
    );

    // And a member this run never had is refused rather than answered about: the
    // caller mistyped, which is not a fact about any note.
    let ghost = MemberName::parse("ghost").expect("a member name");
    let err = control::note(&workspace.state(), &id, &ghost, &note, &oneharness_bin())
        .expect_err("a member the run never had cannot be addressed");
    assert!(err.to_string().contains("has no member \"ghost\""), "{err}");
}

/// A single-sided member has no second party for a note to be addressed away
/// from, so a note to one falls through to the lever it does have — and the
/// addressed role still reaches the turn.
#[cfg(unix)]
#[test]
fn a_note_to_a_single_sided_member_falls_through_to_the_lever_it_has() {
    let _serial = NOTE_RUN.lock().expect("note journey lock");
    let workspace = Workspace::new();
    workspace.graph(&crate::support::single_sided_graph(
        &crate::support::fake_harness(),
    ));
    let mut env = BTreeMap::new();
    env.insert(
        "XDG_STATE_HOME".to_string(),
        workspace.session_store().display().to_string(),
    );
    let request = Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: Some("fake:complete-now: a single-sided member".to_string()),
        dir: workspace.dir(),
        labels: Vec::new(),
        overrides: Vec::new(),
        filter: None,
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };
    let run = run::start(&request, &env).expect("the graph starts");
    let id = run.started().run_id.clone();
    assert_eq!(run.wait().expect("the member settles"), 0);

    let member = MemberName::parse("reporter").expect("a member name");
    let note = Note::new(Addressee::Worker, "one more thing").expect("a note with text in it");
    let delivery = control::note(&workspace.state(), &id, &member, &note, &oneharness_bin())
        .expect("the run and its member are addressable");
    // Settled, so what comes back is the settled refusal rather than a socket
    // asked about a turn nobody is running — reached through the fall-through,
    // which is the point: one call answers for both member kinds.
    assert!(
        matches!(
            &delivery,
            NoteDelivery::Undelivered(Undelivered::MemberSettled { .. })
        ),
        "{delivery:?}"
    );
}

/// The framing a receiving party is handed, which is what a journey above
/// asserts on and what an operator reads in a member's own turn.
#[allow(unused)]
fn framing(note: &Note) -> String {
    note.framed()
}

/// Kept so the unix-only fixtures above type-check on Windows.
#[allow(unused)]
fn unused(path: PathBuf) -> PathBuf {
    path
}
