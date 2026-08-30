//! Role-addressed notes: an update to one party's task, routed to whichever side
//! of a two-party member is live.
//!
//! `interrupt` aims one redirection at the **agent** side's socket, which is why
//! a note delivered that way has only ever reached the worker: the judge has no
//! socket, so a manager's ruling reached the party doing the work and never the
//! party judging it — and that judge went on reviewing against a task the ruling
//! had changed. These journeys are the routed path beside it: the note is offered
//! to the member itself, and the conversation it is actually having decides.
//!
//! The decision is `onejudge`'s, because only the engine driving a conversation
//! knows which side of it is live. What these journeys drive is that seam
//! end to end — a note offered through this crate's API reaches the party that is
//! taking a turn, carrying the role it is addressed to, and **the other party
//! reads it with that party's response**:
//!
//! * offered during the worker's live turn, it reopens that turn carrying the
//!   note, and the judge's own prompt then holds the note beside the worker's
//!   answer to it;
//! * offered during the judge's live turn, the judge's decision is re-taken with
//!   the note in hand, and the note rides that response to the worker;
//! * and where that re-taken decision is completion, the work was passed with the
//!   note in hand — [`Accepted::JudgedWith`], which is not a delivery failure.
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
use oneagentgraph::control::{self, Accepted, Addressee, Note, NoteDelivery, Party, Undelivered};
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

/// Every prompt captured in `at`, one per element.
///
/// The double writes one line per invocation with newlines escaped, so a prompt
/// is a line. Which *side* wrote a line is not decided here: journeys point
/// `fake:record-prompt` and `fake:record-supervisor-prompt` at separate files,
/// because the marker that tells the two sides apart is the double's and having
/// a second copy of it here would be that marker with two sources.
///
/// Where each sentinel goes is not interchangeable either. The standing system
/// prompt is on every *agent* turn and reaches the judge never; the task is
/// embedded in every *supervisor* prompt and reaches an agent turn only when that
/// turn is the one the task opened.
fn prompts(at: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(at)
        .map(|raw| raw.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Where this run's member receives notes, **as the run itself reports it**.
///
/// `control::record` is the published way to ask a member where its note
/// endpoint is, and `Record::notes` is the field that answers — the same pair a
/// caller outside this process reads before offering a note. A journey asks
/// rather than deriving, so what it watches is the endpoint the run named and not
/// a path a test rebuilt from the run's directory layout.
fn note_endpoint(workspace: &Workspace, running: &Running) -> std::path::PathBuf {
    let scratch = workspace
        .state()
        .join(running.id.as_str())
        .join("members")
        .join(running.member.as_str());
    control::record(&scratch)
        .expect("the member recorded a controllable turn")
        .notes
        .expect("a two-party member records the note endpoint its own thread bound")
}

/// Whether a note is still waiting at the endpoint for the member to take it.
fn waiting(spool: &std::path::Path) -> bool {
    std::fs::read_dir(spool)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(".note.json"))
            })
        })
        .unwrap_or(false)
}

/// Block until the member's courier has taken the note off its spool and handed
/// it to the conversation.
///
/// This is what lets a journey hold a turn open, offer a note *into* it, and then
/// release the turn knowing the note really arrived while that side was live —
/// rather than racing the release against the courier and asserting on whichever
/// won. Both waits are bounded and neither is the assertion: what the note
/// reached is asserted afterwards, off the disposition the conversation gave it,
/// so a bound that expired early shows up as the wrong disposition rather than as
/// a journey that quietly proved nothing.
///
/// Two waits because the spool is empty both *before* the note is offered and
/// *after* it is taken. The first watches for it to land, the second for it to
/// go; and if the courier's take beat the first wait entirely, that wait spends
/// its bound and the second returns at once — either way `Notes::send` has been
/// called by the time this returns.
fn handed_over(spool: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline && !waiting(spool) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    while std::time::Instant::now() < deadline && waiting(spool) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Offer one note to a live member through the library, from a thread of its own.
///
/// Every offer in these journeys is backgrounded, because that is what the API
/// does: the call blocks until the *conversation* has disposed of the note, and
/// the conversation cannot move while a journey is holding one of its turns open.
fn offering(
    workspace: &Workspace,
    running: &Running,
    note: Note,
) -> std::thread::JoinHandle<NoteDelivery> {
    let (state, id, member) = (
        workspace.state(),
        running.id.clone(),
        running.member.clone(),
    );
    std::thread::spawn(move || {
        control::note(&state, &id, &member, &note, &oneharness_bin())
            .expect("the run and its member are addressable")
    })
}

/// A note offered during the worker's **live** turn reaches the worker carrying
/// its role, and the judge then reads that note beside the worker's response
/// to it.
///
/// This is the delivery the seam exists for, and both halves are asserted off the
/// prompts the two sides were really handed. The worker's next turn is the note
/// itself — reopened carrying it, so the note is what that turn answers. The
/// judge's own prompt then holds the note *and* the reply the worker gave it,
/// which is the failure this replaces: a ruling that reached the worker at
/// 15:50:23Z and was contradicted seven minutes later by a judge reviewing
/// against a task that never mentioned it.
#[cfg(unix)]
#[test]
fn a_note_during_a_live_worker_turn_reaches_the_worker_and_the_judge_reads_it_with_the_response() {
    let _serial = NOTE_RUN.lock().expect("note journey lock");
    let workspace = Workspace::new();
    let agent_prompts = workspace.at("agent-prompts");
    let judge_prompts = workspace.at("judge-prompts");
    let began = workspace.at("worker-began");
    let release = workspace.at("worker-release");
    let running = start(
        &workspace,
        &format!("fake:record-prompt={}", agent_prompts.display()),
        &format!(
            "fake:record-supervisor-prompt={} fake:entered={} fake:hold={}",
            judge_prompts.display(),
            began.display(),
            release.display()
        ),
    );

    until("the worker's turn to be in flight", || began.exists());

    let text = "fake:complete-now the release blocker is P0: fix it before anything else";
    let note = Note::new(Addressee::Worker, text).expect("a note with text in it");
    let endpoint = note_endpoint(&workspace, &running);
    let events = running.run.started().events_path.clone();
    let offered = offering(&workspace, &running, note);
    handed_over(&endpoint);
    std::fs::write(&release, "go").expect("release the worker's held turn");

    let delivery = offered.join().expect("the offering thread");
    assert_eq!(
        delivery,
        NoteDelivery::Accepted(Accepted::Interrupted {
            party: Party::Worker
        }),
        "a note offered while the worker's turn was live did not reach the worker"
    );

    assert_eq!(
        running.run.wait().expect("the member settles"),
        0,
        "the member did not settle after the note"
    );

    // The worker was handed the note, framed as its own task's update.
    let handed = prompts(&agent_prompts);
    assert!(
        handed
            .iter()
            .any(|prompt| prompt.contains(text) && prompt.contains("delivered to YOU, the worker")),
        "the worker never got a turn carrying the note as its own update: {handed:#?}"
    );

    // And the judge read it beside the worker's answer to it. `done` is that
    // answer — the turn the note opened is the one that finished the work — so a
    // prompt carrying both is a judge that saw the update and the response to it
    // in the same breath, which is the whole point.
    let judged = prompts(&judge_prompts);
    assert!(
        judged.iter().any(|prompt| prompt.contains(text)
            && prompt.contains("delivered to the WORKER, addressed to it and not to you")
            && prompt.contains("done")),
        "the judge never read the note beside the worker's response to it: {judged:#?}"
    );

    // And the run said so on its own stream: a caller learns the disposition from
    // its own call, and this is what an operator reading the journal sees.
    let published = std::fs::read_to_string(&events).expect("the run's own stream");
    let reported: Vec<serde_json::Value> = published
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["kind"] == "turn-interrupted")
        .collect();
    assert_eq!(
        reported.len(),
        1,
        "the delivery was not reported once: {published}"
    );
    assert_eq!(reported[0]["payload"]["delivered"], true);
    assert_eq!(reported[0]["payload"]["member"], "worker");
    assert_eq!(reported[0]["payload"]["reason"], serde_json::Value::Null);
}

/// A note offered while the **judge's** turn is live reaches the judge, and rides
/// its response to the worker.
///
/// This is the case `interrupt` cannot express at all: onejudge opens a
/// controllable turn for the agent side alone, so there is nothing to redirect
/// while the supervisor is deciding, and an `interrupt` here is refused as
/// *between turns*. The routed path hands it to the party that is actually live —
/// whose decision is then re-taken with the note in hand — and the worker
/// receives it together with that decision, in one turn, so it cannot act on the
/// response without the note that shaped it.
///
/// The note **binds**, so this also drives the criterion it adds through to the
/// worker's own framing.
#[cfg(unix)]
#[test]
fn a_note_during_a_live_judge_turn_reaches_the_judge_and_rides_its_response_to_the_worker() {
    let _serial = NOTE_RUN.lock().expect("note journey lock");
    let workspace = Workspace::new();
    let agent_prompts = workspace.at("agent-prompts");
    let judge_prompts = workspace.at("judge-prompts");
    let judging = workspace.at("judge-gate");
    let judge_entered = workspace.at("judge-gate.entered");
    // `should-fail` keeps the supervisor asking for another turn, so the decision
    // the note is delivered into is one that *continues* — which is what carries
    // it to the worker. The run then ends at the base config's turn cap.
    let running = start(
        &workspace,
        &format!("fake:record-prompt={}", agent_prompts.display()),
        &format!(
            "fake:record-supervisor-prompt={} fake:should-fail fake:supervisor-hold={}",
            judge_prompts.display(),
            judging.display()
        ),
    );

    until("the judge's own turn to be in flight", || {
        judge_entered.exists()
    });

    let text = "the acceptance bar moved: the migration has to be reversible";
    let note = Note::new(Addressee::Worker, text)
        .expect("a note with text in it")
        .binding("the migration is reversible")
        .expect("a criterion a judge can hold work to");
    let endpoint = note_endpoint(&workspace, &running);
    let offered = offering(&workspace, &running, note);
    handed_over(&endpoint);
    std::fs::write(&judging, "go").expect("release the judge's held turn");

    let delivery = offered.join().expect("the offering thread");
    assert_eq!(
        delivery,
        NoteDelivery::Accepted(Accepted::Interrupted {
            party: Party::Supervisor
        }),
        "a note offered while the judge was deciding did not reach the judge"
    );

    running.run.wait().expect("the member settles");

    // The judge's re-taken decision was taken with the note in hand.
    let judged = prompts(&judge_prompts);
    assert!(
        judged.iter().any(|prompt| prompt.contains(text)),
        "the judge decided without ever being shown the note: {judged:#?}"
    );

    // And the worker got it with that decision: the note's own text, the role it
    // is addressed to, the criterion it bound, and the supervisor's words, in one
    // turn.
    let handed = prompts(&agent_prompts);
    assert!(
        handed.iter().any(|prompt| prompt.contains(text)
            && prompt.contains("delivered to YOU, the worker")
            && prompt.contains("the migration is reversible")
            && prompt.contains("verify it before you call it done")),
        "the note never rode the judge's response to the worker: {handed:#?}"
    );
}

/// A note the judge passes the work with is accepted as exactly that, rather than
/// reported undelivered.
///
/// The judge's live decision is re-taken with the note in hand, and when that
/// re-taken decision is completion there is no next worker turn for the note to
/// ride: the work was passed *with* it. That is [`Accepted::JudgedWith`], and it
/// is an acceptance rather than a failure — a caller holding one knows the note
/// changed nothing and the member needs no relaunch.
///
/// Addressed to the **supervisor**, which is the party it reaches: the judge is
/// told the update is for it, by name, so it does not read the worker's amendment
/// as its own next instruction.
#[cfg(unix)]
#[test]
fn a_note_the_judge_passed_the_work_with_is_accepted_as_judged_with() {
    let _serial = NOTE_RUN.lock().expect("note journey lock");
    let workspace = Workspace::new();
    let judge_prompts = workspace.at("judge-prompts");
    let judging = workspace.at("judge-gate");
    let judge_entered = workspace.at("judge-gate.entered");
    let running = start(
        &workspace,
        "",
        &format!(
            "fake:record-supervisor-prompt={} fake:supervisor-hold={}",
            judge_prompts.display(),
            judging.display()
        ),
    );

    until("the judge's own turn to be in flight", || {
        judge_entered.exists()
    });

    let text = "hold this to the amended bar: reversibility is now in scope";
    let note = Note::new(Addressee::Supervisor, text).expect("a note with text in it");
    let endpoint = note_endpoint(&workspace, &running);
    let offered = offering(&workspace, &running, note);
    handed_over(&endpoint);
    std::fs::write(&judging, "go").expect("release the judge's held turn");

    let delivery = offered.join().expect("the offering thread");
    assert!(
        matches!(
            &delivery,
            NoteDelivery::Accepted(Accepted::JudgedWith { completion_reason })
                if completion_reason.contains("fake supervisor verified completion")
        ),
        "a note the judge passed the work holding was not reported as judged with it: {delivery:?}"
    );

    assert_eq!(
        running.run.wait().expect("the member settles"),
        0,
        "the member did not settle after the note"
    );

    // The judge really was shown it, addressed to itself by name.
    let judged = prompts(&judge_prompts);
    assert!(
        judged.iter().any(|prompt| prompt.contains(text)
            && prompt.contains("delivered to YOU, the supervisor")
            && prompt.contains("(addressed to supervisor)")),
        "the judge passed the work without being shown the note it was addressed by: {judged:#?}"
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
/// The refusal driven here is the settled member, which is the one an operator
/// actually meets. Its sibling — a conversation that reached its completion
/// decision — is the same terminal record read a moment earlier, and is driven
/// across the real spool and the real `submit` by
/// `note::tests::a_member_that_stops_taking_notes_refuses_them_rather_than_accepting_one`,
/// which drives both refusals through the code path this call ends in.
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

/// A note offered while **no turn is live** is held for the next turn to open,
/// and arrives there carrying its role unchanged.
///
/// The third of the three states a conversation can be in when a note reaches
/// it, and the only one no harness can hold: a harness runs *inside* a turn, so
/// it cannot pause the gap between two. `judge`'s `hold_between_turns` fixture —
/// behind the non-default `test-doubles` feature, with the rest of what only this
/// suite needs — pauses the supervisor's turn boundary, which is the gap before
/// the next worker turn opens. Everything else here is the same real run as its
/// siblings: the note goes through the public `control::note`, the conversation
/// decides, and what the worker was handed is read off the prompt it really got.
#[cfg(unix)]
#[test]
fn a_note_offered_between_turns_is_held_for_the_next_one_and_arrives_carrying_its_role() {
    let _serial = NOTE_RUN.lock().expect("note journey lock");
    let workspace = Workspace::new();
    let agent_prompts = workspace.at("agent-prompts");
    let between = workspace.at("between-turns");
    let between_entered = workspace.at("between-turns.entered");
    // Read by the sink on this run's own engine thread, which is a thread of this
    // process — so it is set here rather than in the graph's `env:`, which is
    // exported to member processes only. Safe to set for the whole process:
    // nextest runs each test in one of its own.
    std::env::set_var(
        "ONEAGENTGRAPH_FIXTURE_HOLD_BETWEEN_TURNS",
        between.display().to_string(),
    );

    // `should-fail` keeps the supervisor asking for another turn, so there *is* a
    // next worker turn for the held note to be delivered into. The run then ends
    // at the base config's turn cap.
    let running = start(
        &workspace,
        &format!("fake:record-prompt={}", agent_prompts.display()),
        "fake:should-fail",
    );

    // The supervisor's first turn has closed and the next worker turn has not
    // opened: no turn is live, and the engine is held right there.
    until("the conversation to reach a turn boundary", || {
        between_entered.exists()
    });

    let text = "no turn was running when this was sent: take it on the next one";
    let note = Note::new(Addressee::Worker, text).expect("a note with text in it");
    let endpoint = note_endpoint(&workspace, &running);
    let offered = offering(&workspace, &running, note);
    handed_over(&endpoint);
    std::fs::write(&between, "go").expect("release the held turn boundary");

    let delivery = offered.join().expect("the offering thread");
    assert_eq!(
        delivery,
        NoteDelivery::Accepted(Accepted::Queued),
        "a note offered with no turn live was not held for the next turn to open"
    );

    running.run.wait().expect("the member settles");

    // And the next worker turn is where it arrived, with the role it was
    // addressed to unchanged.
    let handed = prompts(&agent_prompts);
    assert!(
        handed
            .iter()
            .any(|prompt| prompt.contains(text) && prompt.contains("delivered to YOU, the worker")),
        "the held note never reached the next worker turn: {handed:#?}"
    );
}
