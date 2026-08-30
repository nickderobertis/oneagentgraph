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

// llmlint: ignore-file[expensive_tests_stay_behind_their_own_edge] this crate has
// exactly one e2e target — `[[test]] name = "e2e"` in Cargo.toml — and all eleven
// journey files are modules of it. There is no narrower edge for these to sit
// behind and no way to make one without splitting that target, which is a change
// to how the whole suite is scheduled rather than anything this file decides.
// These journeys also cost what their siblings cost: each drives one short run of
// the same doubled harness, and the whole file finishes in seconds.

// The routed delivery reaches a turn through a unix domain socket, so the
// journeys that drive one are `cfg(unix)` exactly as the interrupt journeys are.
// The fixtures below are still compiled on Windows — type-checked and linted —
// with nothing calling them.
#![allow(dead_code)]

use std::collections::BTreeMap;

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

    // A note whose *only* addressee is the supervisor, first: the judge has no
    // out-of-band lever and the conversation layer this build links composes its
    // effective task before the run starts, so there is nothing to hand it to.
    // Refused naming that, rather than delivered to the worker under a frame
    // addressed to somebody else — which would be the note's addressee never
    // seeing it and the wrong party acting on it.
    let for_judge = Note::new(Addressee::Supervisor, "judge this against the amended bar")
        .expect("a note with text in it");
    let refused = offer(&workspace, &running, &for_judge);
    assert!(
        matches!(
            &refused,
            NoteDelivery::Undelivered(Undelivered::NoConversation { reason })
                if reason.contains("supervisor")
        ),
        "a note only the supervisor was addressed by was not refused: {refused:?}"
    );

    // And a blank one is refused before anything is routed: an update with
    // nothing in it is not one a party can act on.
    let blank = Note {
        addressee: Addressee::Worker,
        text: "   ".to_string(),
        binds: false,
    };
    let empty = control::note(
        &workspace.state(),
        &running.id,
        &running.member,
        &blank,
        &oneharness_bin(),
    )
    .expect_err("a note with nothing in it cannot be routed");
    assert!(empty.to_string().contains("a note needs text"), "{empty}");

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

/// A note that cannot be delivered comes back as an [`Undelivered`] the caller
/// has to read, rather than as an acceptance.
///
/// That is the failure the whole seam exists to remove: a note taken into a
/// member nothing would ever read it out of looks, to the caller, exactly like
/// one that landed. [`Undelivered`] is a `std::error::Error` and every arm's
/// `Display` opens with *"the note was not delivered"*, so a caller that only
/// prints it still learns the fact.
///
/// The refusal driven here is the settled member. Its sibling — a conversation
/// that reached its completion decision — is unreachable through this API by
/// construction and is driven where it is real; the body says where, and why.
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

    // The other refusal — a conversation that reached its completion decision —
    // is the member's own end of the same seam and is driven across the real
    // `Router::complete` and the real `submit` by
    // `note::tests::a_completed_conversation_refuses_a_note_rather_than_accepting_it`.
    // It cannot be reached through this API from outside, and closing it is what
    // made it unreachable: a member records its completion and then settles, and
    // the call above answers a settled member before it ever asks the spool.

    // A member this run never had is refused rather than answered about: the
    // caller mistyped, which is not a fact about any note.
    let ghost = MemberName::parse("ghost").expect("a member name");
    let err = control::note(&workspace.state(), &id, &ghost, &note, &oneharness_bin())
        .expect_err("a member the run never had cannot be addressed");
    assert!(err.to_string().contains("has no member \"ghost\""), "{err}");
}

/// A single-sided member has no second party for a note to be addressed away
/// from, so a note to one falls through to the lever it does have.
///
/// What that lever answers is the assertion: this member kind never asks for a
/// controllable turn — only a two-party member's agent side does — so a note to a
/// *live* one comes back naming that, and one to a settled one comes back naming
/// the settle. Both are [`Undelivered`], and neither is the endpoint path: the
/// point is that one call answers for both member kinds rather than refusing to
/// address half of them.
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
    let began = workspace.at("began");
    let release = workspace.at("release");
    let request = Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: Some(format!(
            "fake:complete-now fake:entered={} fake:hold={}",
            began.display(),
            release.display()
        )),
        dir: workspace.dir(),
        labels: Vec::new(),
        overrides: Vec::new(),
        filter: None,
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };
    let run = run::start(&request, &env).expect("the graph starts");
    let id = run.started().run_id.clone();
    let member = MemberName::parse("reporter").expect("a member name");
    let note = Note::new(Addressee::Worker, "one more thing").expect("a note with text in it");

    // While the member's turn is genuinely in flight, so what answers is the
    // fall-through rather than the run's own record of a member that is over: a
    // single-sided member binds no note endpoint, so the call goes to the lever
    // it does have and reports what that lever says.
    until("the single-sided member's turn to be in flight", || {
        began.exists()
    });
    let live = control::note(&workspace.state(), &id, &member, &note, &oneharness_bin())
        .expect("the run and its member are addressable");
    assert!(
        matches!(
            &live,
            NoteDelivery::Undelivered(Undelivered::NoConversation { reason })
                if reason.contains("controllable turn")
        ),
        "a live single-sided member did not answer through the lever it has: {live:?}"
    );

    std::fs::write(&release, "go").expect("release the member");
    assert_eq!(run.wait().expect("the member settles"), 0);

    // And once it is over, the run's own record answers first — a settled member
    // is not asked about a turn nobody is running.
    let settled = control::note(&workspace.state(), &id, &member, &note, &oneharness_bin())
        .expect("the run and its member are addressable");
    assert!(
        matches!(
            &settled,
            NoteDelivery::Undelivered(Undelivered::MemberSettled { .. })
        ),
        "{settled:?}"
    );
}

/// A queued note the conversation completes past is reported on the run's own
/// stream rather than quietly dropped.
///
/// `Accepted::Queued` promises the next worker turn, and a supervisor that ends
/// the conversation instead means that turn never comes. The caller was told the
/// note was taken, so the run has to say what became of it: one
/// `turn-interrupted` naming that it was **not** delivered and that the
/// conversation completed first. Without it a caller reads a queued note exactly
/// as it reads a delivered one, which is the silence this whole seam replaces.
#[cfg(unix)]
#[test]
fn a_queued_note_the_conversation_completes_past_is_reported_as_undelivered() {
    let _serial = NOTE_RUN.lock().expect("note journey lock");
    let workspace = Workspace::new();
    let judging = workspace.at("judge-gate");
    let judge_entered = workspace.at("judge-gate.entered");
    let running = start(
        &workspace,
        "",
        &format!(
            "fake:complete-now fake:supervisor-hold={}",
            judging.display()
        ),
    );

    until("the judge's own turn to be in flight", || {
        judge_entered.exists()
    });
    let note = Note::new(Addressee::Worker, "one more thing before you finish")
        .expect("a note with text in it");
    assert_eq!(
        offer(&workspace, &running, &note),
        NoteDelivery::Accepted(Accepted::Queued),
        "a note offered while the judge was deciding was not queued"
    );

    // The judge answers completion, so there is no next worker turn for the
    // queued note to ride.
    std::fs::write(&judging, "go").expect("release the judge");
    let events = running.run.started().events_path.clone();
    assert_eq!(
        running.run.wait().expect("the member settles"),
        0,
        "the member did not settle"
    );

    let published = std::fs::read_to_string(&events).expect("the run's own stream");
    let undelivered: Vec<serde_json::Value> = published
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["kind"] == "turn-interrupted")
        .collect();
    assert_eq!(
        undelivered.len(),
        1,
        "the queued note was not reported once: {published}"
    );
    assert_eq!(undelivered[0]["payload"]["delivered"], false);
    assert_eq!(undelivered[0]["payload"]["member"], "worker");
    assert!(
        undelivered[0]["payload"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("never delivered")),
        "the report did not say the note was never delivered: {}",
        undelivered[0]
    );
}
