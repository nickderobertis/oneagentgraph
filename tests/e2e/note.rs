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
//! The note is **offered** first ([`oneagentgraph::control::offer_note`]) and its
//! answer waited on after, so a caller learns it is in before any turn answers it,
//! bounds that wait with a deadline, or takes the note back — and the run's own
//! stream says each of those as it happens.
//!
//! Every journey here is driven through the **library**, because that is what the
//! seam is required on: a consumer of this crate calls
//! [`oneagentgraph::control::offer_note`] and never types a verb. The `interrupt`
//! verb's own journeys are in `tests/e2e/interrupt.rs`.

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
// What only a routed delivery names, and so only the `cfg(unix)` journeys below
// use: the dispositions a live turn answers with, and the wait for one to be in
// flight. Split off rather than left in the list beside it, the way
// `tests/e2e/interrupt.rs` splits its own — on Windows these names are compiled
// with nothing referring to them, and an unused import is `-D warnings`.
use oneagentgraph::control::{self, Addressee, Note, NoteDelivery, Undelivered};
#[cfg(unix)]
use oneagentgraph::control::{Accepted, NoteDeliveryMode, NoteOffer, NoteOffered, Party};
use oneagentgraph::run::{self, MemberName, Request};

#[cfg(unix)]
use crate::support::until;
use crate::support::{
    oneharness_bin, shed_inherited_environment, two_party_graph, Workspace, BASE,
};

/// Journeys here start real runs with real session stores; one at a time, as the
/// library journeys next door are, so two runs never race for the same host.
static NOTE_RUN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Hold [`NOTE_RUN`] for one journey, having shed what this process must not
/// hand the in-process turn: these journeys' graphs run in this process, so its
/// environment is the one the core reads.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    let guard = NOTE_RUN.lock().expect("note journey lock");
    shed_inherited_environment();
    guard
}

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
    let mut document: serde_json::Value =
        oneagentgraph::yaml::from_str(BASE).expect("the base is a YAML document");
    let mapping = document.as_object_mut().expect("the base is a mapping");
    let standing = mapping
        .get("system_prompt")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    mapping.insert(
        "system_prompt".to_string(),
        serde_json::Value::String(format!("{standing}\n{system}\n")),
    );
    oneagentgraph::yaml::to_string(&document).expect("the base serializes")
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

/// How long a journey gives an offer to reach the conversation before it releases
/// the turn it is holding open.
///
/// An **ordering** margin, not a wait for a result: the offer runs on a thread of
/// its own and blocks until the conversation disposes of the note, and the
/// conversation cannot move while a journey holds one of its turns. This is the
/// window in which `control::note` writes the note down and the member picks it
/// up — tens of milliseconds in practice, against a bound an order of magnitude
/// past it, on journeys that finish in under a second.
///
/// Deliberately not a poll of the member's own files. What the note reached is
/// asserted afterwards off the disposition the *conversation* gave it, so a
/// margin that was ever too short shows up as the wrong party in that assertion —
/// a red journey rather than a green one that proved nothing.
const REACHES_THE_CONVERSATION: std::time::Duration = std::time::Duration::from_secs(2);

/// Wait [`REACHES_THE_CONVERSATION`] out; that constant carries why, and why the
/// name is the wait rather than what the wait is for.
fn wait_the_ordering_margin() {
    std::thread::sleep(REACHES_THE_CONVERSATION);
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
    let _serial = serial();
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
    let events = running.run.started().events_path.clone();
    let offered = offering(&workspace, &running, note);
    wait_the_ordering_margin();
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

/// A note for the **worker** offered while the judge's turn is live reaches the
/// judge once that turn ends, and rides its response to the worker.
///
/// The judge's turn is not the addressee's, so the note does not redirect it:
/// it is [`Accepted::AtTurnEnd`] naming the supervisor, whose decision is re-taken
/// with the note in hand — and the worker receives it together with that
/// decision, in one turn, so it cannot act on the response without the note that
/// shaped it. A note addressed to the supervisor itself *does* redirect that
/// turn; [`a_supervisor_note_redirects_the_live_decision_and_a_turn_end_one_waits_for_it_to_end`]
/// drives that.
///
/// The note **binds**, so this also drives the criterion it adds through to the
/// worker's own framing.
#[cfg(unix)]
#[test]
fn a_note_during_a_live_judge_turn_reaches_the_judge_and_rides_its_response_to_the_worker() {
    let _serial = serial();
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
    let offered = offering(&workspace, &running, note);
    wait_the_ordering_margin();
    std::fs::write(&judging, "go").expect("release the judge's held turn");

    let delivery = offered.join().expect("the offering thread");
    assert_eq!(
        delivery,
        NoteDelivery::Accepted(Accepted::AtTurnEnd {
            party: Party::Supervisor
        }),
        "a worker's note offered while the judge was deciding did not reach the judge at its end"
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
    let _serial = serial();
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
    let offered = offering(&workspace, &running, note);
    wait_the_ordering_margin();
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
/// decision — is driven through the same public call by
/// [`a_note_after_the_conversation_completed_names_that_rather_than_the_member_settling`],
/// which is the journey below.
#[test]
fn a_note_that_cannot_be_delivered_says_so_rather_than_being_accepted() {
    let _serial = serial();
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

/// A note offered after the conversation **completed** is refused as
/// [`Undelivered::ConversationCompleted`], naming the supervisor's own decision
/// rather than a settle.
///
/// A caller acts on the difference: a conversation its supervisor *passed* needs
/// no relaunch and the note is a follow-up, while a member the run merely settled
/// may well be worth starting again. So the two are separate arms, and this
/// drives the one the run's own record would otherwise answer over.
///
/// Reaching it needs a run that is **still going** once the conversation is over,
/// because `control::note` reads the run's record first and a member that record
/// has settled is answered from there without the member being asked. A second
/// member holding its own turn open behind the two-party one is that window, and
/// it is an ordinary graph rather than a fixture: `holder` declares `deps:
/// [worker]`, so it only opens its turn once the conversation has completed and
/// its terminal refusal is recorded, and it holds that turn while the note is
/// offered.
#[cfg(unix)]
#[test]
fn a_note_after_the_conversation_completed_names_that_rather_than_the_member_settling() {
    let _serial = serial();
    let workspace = Workspace::new();
    let began = workspace.at("holder-began");
    let release = workspace.at("holder-release");

    workspace.graph(&crate::support::graph_with(
        concat!(
            "version: 3\nname: node-scope\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n",
            "  holder:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    deps: [worker]\n",
        ),
        &[
            (
                crate::support::FAKE_HARNESS_KEY.to_string(),
                crate::support::fake_harness(),
            ),
            (
                "env.XDG_STATE_HOME".to_string(),
                workspace.session_store().display().to_string(),
            ),
            (
                "members.worker.task".to_string(),
                "fake:complete-now: work the supervisor passes".to_string(),
            ),
            (
                "members.holder.task".to_string(),
                format!(
                    "fake:complete-now fake:entered={} fake:hold={}",
                    began.display(),
                    release.display()
                ),
            ),
        ],
    ));
    let mut env = BTreeMap::new();
    env.insert(
        "XDG_STATE_HOME".to_string(),
        workspace.session_store().display().to_string(),
    );
    let request = Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: None,
        dir: workspace.dir(),
        labels: Vec::new(),
        overrides: Vec::new(),
        filter: None,
        state_dir: workspace.state(),
        oneharness_bin: oneharness_bin(),
    };
    let run = run::start(&request, &env).expect("the graph starts");
    let id = run.started().run_id.clone();
    let member = MemberName::parse("worker").expect("a member name");

    // The dependent member's turn is in flight, which is only reachable once the
    // conversation before it completed: the run is still going, and the record
    // has nothing to say about `worker` yet.
    until("the dependent member's turn to be in flight", || {
        began.exists()
    });
    let note = Note::new(Addressee::Worker, "one more thing").expect("a note with text in it");
    let after = control::note(&workspace.state(), &id, &member, &note, &oneharness_bin())
        .expect("the run and its member are addressable");
    assert!(
        matches!(
            &after,
            NoteDelivery::Undelivered(Undelivered::ConversationCompleted { completion_reason })
                if !completion_reason.is_empty()
        ),
        "a note after the conversation completed was not refused as completed: {after:?}"
    );
    let NoteDelivery::Undelivered(undelivered) = &after else {
        unreachable!("the match above")
    };
    assert!(
        undelivered.to_string().contains("was not delivered"),
        "the refusal did not say the note was not delivered: {undelivered}"
    );

    std::fs::write(&release, "go").expect("release the holder");
    assert_eq!(run.wait().expect("the graph settles"), 0);
}

/// Start a graph whose one single-sided member is `reporter`, on `task`.
///
/// `chain` is the member's own oneharness config: which harnesses its turn may
/// be served by, which is what decides whether that turn opens with control.
fn start_single_sided(workspace: &Workspace, chain: Option<&str>, task: &str) -> Running {
    if let Some(chain) = chain {
        workspace.write("oneharness.toml", chain);
    }
    workspace.graph(&crate::support::graph_with(
        concat!(
            "version: 1\nname: node-scope\n",
            "env: {}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        &[
            (
                crate::support::FAKE_HARNESS_KEY.to_string(),
                crate::support::fake_harness(),
            ),
            (
                "env.ONEHARNESS_BIN_QWEN".to_string(),
                crate::support::fake_harness(),
            ),
            (
                "env.XDG_STATE_HOME".to_string(),
                workspace.session_store().display().to_string(),
            ),
        ],
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
        member: MemberName::parse("reporter").expect("a member name"),
    }
}

/// A one-identity `qwen` chain: a harness oneharness has no turn control for,
/// which takes the wire shape the double speaks — so a member on it runs for real
/// and only the lever is missing.
const QWEN_CHAIN: &str = "run_mode = \"fallback\"\nharnesses = [\"qwen\"]\n";

/// A single-sided member on a harness oneharness interrupts in place has one
/// party and one lever, and a note to it is carried by that lever — answered
/// [`Accepted::Interrupted`] only because `oneharness interrupt` reported the
/// redirection delivered, and handed to the replacement turn exactly once.
#[cfg(unix)]
#[test]
fn a_note_to_a_single_sided_member_interrupts_it_when_its_lever_reports_the_redirect_delivered() {
    let _serial = serial();
    let workspace = Workspace::new();
    let recorded = workspace.at("prompts");
    let began = workspace.at("began");
    let release = workspace.at("release");
    let running = start_single_sided(
        &workspace,
        None,
        &format!(
            "fake:complete-now fake:record-prompt={} fake:entered={} fake:hold={}",
            recorded.display(),
            began.display(),
            release.display()
        ),
    );
    until("the single-sided member's turn to be in flight", || {
        began.exists()
    });

    let phrase = "fix the P0 before anything else";
    let note = Note::new(
        Addressee::Worker,
        format!("fake:record-prompt={} {phrase}", recorded.display()),
    )
    .expect("a note with text in it");
    let delivery = control::note(
        &workspace.state(),
        &running.id,
        &running.member,
        &note,
        &oneharness_bin(),
    )
    .expect("the run and its member are addressable");
    assert_eq!(
        delivery,
        NoteDelivery::Accepted(Accepted::Interrupted {
            party: Party::Worker
        }),
        "a note to a live single-sided member was not carried by its lever"
    );
    std::fs::write(&release, "go").expect("release the member");
    assert_eq!(running.run.wait().expect("the member settles"), 0);

    let carrying: Vec<String> = prompts(&recorded)
        .into_iter()
        .filter(|prompt| prompt.contains(phrase))
        .collect();
    assert_eq!(
        carrying.len(),
        1,
        "the note did not reach exactly one turn: {carrying:#?}"
    );
    assert_eq!(
        carrying[0].matches(phrase).count(),
        1,
        "the replacement turn was handed the note more than once: {}",
        carrying[0]
    );
    assert!(
        carrying[0].contains("addressed to: worker"),
        "the replacement turn was not told whose task the note updates: {}",
        carrying[0]
    );
}

/// When the lever reports the redirection **not** delivered — the turn ended
/// before oneharness could write it — the note is [`Undelivered`], carrying that
/// refusal's reason, and never [`Accepted::Interrupted`]: no turn ever reads it.
#[cfg(unix)]
#[test]
fn a_note_whose_redirect_the_lever_could_not_deliver_is_undelivered_with_its_reason() {
    let _serial = serial();
    let workspace = Workspace::new();
    let recorded = workspace.at("prompts");
    let began = workspace.at("began");
    let release = workspace.at("release");
    let running = start_single_sided(
        &workspace,
        None,
        &format!(
            "fake:complete-now fake:vanish-on-interrupt fake:record-prompt={} fake:entered={} \
             fake:hold={}",
            recorded.display(),
            began.display(),
            release.display()
        ),
    );
    until("the single-sided member's turn to be in flight", || {
        began.exists()
    });

    let phrase = "this redirect has nowhere to land";
    let note = Note::new(
        Addressee::Worker,
        format!("fake:record-prompt={} {phrase}", recorded.display()),
    )
    .expect("a note with text in it");
    let delivery = control::note(
        &workspace.state(),
        &running.id,
        &running.member,
        &note,
        &oneharness_bin(),
    )
    .expect("the run and its member are addressable");
    let NoteDelivery::Undelivered(Undelivered::NoConversation { reason }) = &delivery else {
        panic!("a redirect the lever could not deliver was answered {delivery:?}");
    };
    assert!(
        reason.contains("turn has already ended") || reason.contains("did not deliver"),
        "the refusal's own reason did not reach the caller: {reason}"
    );
    std::fs::write(&release, "go").expect("release the member");
    running.run.wait().expect("the member settles");

    assert!(
        prompts(&recorded)
            .iter()
            .all(|prompt| !prompt.contains(phrase)),
        "a note reported undelivered was read by a turn: {:#?}",
        prompts(&recorded)
    );
}

/// A turn-end note to a single-sided member is refused rather than turned into
/// the interrupt it asked not to be: holding a note for a turn's end is a
/// conversation's to do, and this member has none. The held turn runs to its end
/// untouched, and no turn reads the note.
#[cfg(unix)]
#[test]
fn a_turn_end_note_to_a_single_sided_member_is_refused_and_its_turn_left_alone() {
    let _serial = serial();
    let workspace = Workspace::new();
    let recorded = workspace.at("prompts");
    let did_work = workspace.at("did-work");
    let began = workspace.at("began");
    let release = workspace.at("release");
    let running = start_single_sided(
        &workspace,
        None,
        &format!(
            "fake:complete-now fake:record-prompt={} fake:did-work={} fake:entered={} fake:hold={}",
            recorded.display(),
            did_work.display(),
            began.display(),
            release.display()
        ),
    );
    until("the single-sided member's turn to be in flight", || {
        began.exists()
    });

    let phrase = "after this turn, rebase onto main";
    let note = Note::new(Addressee::Worker, phrase)
        .expect("a note with text in it")
        .with_delivery(NoteDeliveryMode::TurnEnd);
    let offered = control::offer_note(
        &workspace.state(),
        &running.id,
        &running.member,
        &note,
        &oneharness_bin(),
    )
    .expect("the run and its member are addressable");
    let NoteOffered::Undelivered(Undelivered::NoConversation { reason }) = &offered else {
        panic!("a turn-end note to a single-sided member was answered {offered:?}");
    };
    assert!(
        reason.contains("turn-end delivery needs a two-party member"),
        "the refusal did not say why: {reason}"
    );

    std::fs::write(&release, "go").expect("release the member");
    assert_eq!(running.run.wait().expect("the member settles"), 0);
    // `did-work` is written only by a turn that got past its hold and was not
    // stopped there: the held turn ran to its own end.
    assert!(
        std::fs::read_to_string(&did_work).is_ok_and(|work| work.contains("fake:hold")),
        "the held turn did not run to completion — something interrupted it"
    );
    assert!(
        prompts(&recorded)
            .iter()
            .all(|prompt| !prompt.contains(phrase)),
        "a refused note was read by a turn: {:#?}",
        prompts(&recorded)
    );
}

/// A single-sided member whose chain oneharness cannot interrupt in place has no
/// lever at all, and a note to it says so — it is never reported interrupted.
#[cfg(unix)]
#[test]
fn a_note_to_a_single_sided_member_with_no_lever_says_so_rather_than_interrupting() {
    let _serial = serial();
    let workspace = Workspace::new();
    let began = workspace.at("began");
    let release = workspace.at("release");
    let running = start_single_sided(
        &workspace,
        Some(QWEN_CHAIN),
        &format!(
            "fake:complete-now fake:entered={} fake:hold={}",
            began.display(),
            release.display()
        ),
    );
    until("the single-sided member's turn to be in flight", || {
        began.exists()
    });

    let note = Note::new(Addressee::Worker, "one more thing").expect("a note with text in it");
    let live = control::note(
        &workspace.state(),
        &running.id,
        &running.member,
        &note,
        &oneharness_bin(),
    )
    .expect("the run and its member are addressable");
    assert!(
        matches!(
            &live,
            NoteDelivery::Undelivered(Undelivered::NoConversation { reason })
                if reason.contains("opened no controllable turn") && reason.contains("qwen")
        ),
        "a single-sided member with no lever did not say so: {live:?}"
    );

    std::fs::write(&release, "go").expect("release the member");
    assert_eq!(running.run.wait().expect("the member settles"), 0);

    // And once it is over, the run's own record answers first — a settled member
    // is not asked about a turn nobody is running.
    let settled = control::note(
        &workspace.state(),
        &running.id,
        &running.member,
        &note,
        &oneharness_bin(),
    )
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
/// The third of the three states a conversation can be in when a note reaches it.
/// It is an ordinary state — every run passes through it between every pair of
/// turns — and the only one no harness can *hold*, because a harness runs inside
/// a turn and cannot pause the gap between two.
///
/// So the timing is held rather than the state invented, which is exactly what
/// `fake:hold` does for a live turn in the journeys above: `judge`'s
/// `hold_between_turns`, behind the non-default `test-doubles` feature with the
/// rest of what only this suite needs, waits at the supervisor's turn boundary —
/// the gap before the next worker turn opens. Nothing about the conversation is
/// simulated: what the note meets is a real engine at a real boundary, reached by
/// running the turns that lead to it, and the fixture is asked for in the task —
/// the same surface the `fake:` sentinels above are given on. Everything else is
/// the same real run as its siblings: the note goes through the public
/// `control::note`, the conversation decides, and what the worker was handed is
/// read off the prompt it really got.
#[cfg(unix)]
#[test]
fn a_note_offered_between_turns_is_held_for_the_next_one_and_arrives_carrying_its_role() {
    let _serial = serial();
    let workspace = Workspace::new();
    let agent_prompts = workspace.at("agent-prompts");
    let between = workspace.at("between-turns");
    let between_entered = workspace.at("between-turns.entered");
    // The gate is asked for in the **task**, which is the same lever every journey
    // above steers a conversation with — the harness double's `fake:` sentinels
    // reach it the same way. `should-fail` keeps the supervisor asking for another
    // turn, so there *is* a next worker turn for the held note to be delivered
    // into; the run then ends at the base config's turn cap.
    let running = start(
        &workspace,
        &format!("fake:record-prompt={}", agent_prompts.display()),
        &format!(
            "fake:should-fail oneagentgraph-fixture:hold-between-turns={}",
            between.display()
        ),
    );

    // The supervisor's first turn has closed and the next worker turn has not
    // opened: no turn is live, and the engine is held right there.
    until("the conversation to reach a turn boundary", || {
        between_entered.exists()
    });

    let text = "no turn was running when this was sent: take it on the next one";
    let note = Note::new(Addressee::Worker, text).expect("a note with text in it");
    let offered = offering(&workspace, &running, note);
    wait_the_ordering_margin();
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

/// A task naming a gate this fixture will not write beside runs **unheld**, and
/// writes nothing.
///
/// The refusal beside the acceptance above, driven the same way: through a real
/// conversation, with the gate asked for in the task. A relative name is the case
/// that matters, because it is the one that resolves — just not where the journey
/// meant. The fixture runs on a thread of the *hosting* process, whose directory
/// is deliberately not the member's (`library::the_hosting_process_directory_never_moves_for_a_member_that_works_elsewhere`
/// pins that), so an unchecked relative gate would have this suite writing a
/// stray file into whatever directory the test runner happened to start in and
/// then holding the conversation for the whole fixture bound waiting on it.
///
/// So the assertion is the two halves of that: the run settles on its own rather
/// than at a hold nobody will release, and no `.entered` marker appears beside
/// this process.
#[cfg(unix)]
#[test]
fn a_gate_named_relatively_is_refused_and_the_conversation_runs_unheld() {
    let _serial = serial();
    let workspace = Workspace::new();
    // Its own name, so what this asserts the absence of is this journey's alone.
    let relative = "oneagentgraph-relative-gate-check";
    let stray = std::env::current_dir()
        .expect("the hosting process has a working directory")
        .join(format!("{relative}.entered"));
    assert!(
        !stray.exists(),
        "the marker this journey asserts the absence of was already there: {}",
        stray.display()
    );

    let running = start(
        &workspace,
        "",
        &format!("fake:should-fail oneagentgraph-fixture:hold-between-turns={relative}"),
    );
    // Settles at the base config's turn cap, exactly as the journey above does
    // once its hold is released — here there was never a hold to release.
    running.run.wait().expect("the member settles");

    assert!(
        !stray.exists(),
        "a relative gate was written beside the hosting process: {}",
        stray.display()
    );
}

/// Every envelope of one `kind` on a run's own stream, in the order written.
fn published_of_kind(published: &str, kind: &str) -> Vec<serde_json::Value> {
    published
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["kind"] == kind)
        .collect()
}

/// The `origin` a turn payload was stamped with, if it was stamped.
///
/// Read as a string, so a payload that wrote `null` for it reads as no origin
/// here and is caught by an absent-key assertion rather than silently
/// satisfying one of the stamps.
fn origin(event: &serde_json::Value) -> Option<String> {
    event["payload"]["origin"].as_str().map(str::to_string)
}

/// Every turn a conversation publishes names who authored what it carries: the
/// composed task, the supervising side's own words, or text a caller handed this
/// graph to deliver.
///
/// The three reach a consumer as the same envelope otherwise — one naming a role
/// and nothing else — so a consumer deciding anything on the strength of the
/// distinction has to guess, and one such guess cancelled a live dispatch on a
/// stop order no operator had given. This graph is the only party that can say:
/// it composed the task, it runs the supervising side, and this file's own seam
/// is the delivery path.
///
/// One run carries all four facts. The worker's first turn opens on the task and
/// is held; a turn-end note is offered into that live turn, so once it ends the
/// engine opens the worker's next turn carrying the caller's words — a note that
/// *interrupted* the turn would instead redirect it in place, on the turn already
/// announced; `should-fail` then keeps the supervisor asking for
/// another turn, which is what puts its own words on the stream and opens a
/// worker turn on them. The fourth is what is **not** stamped — an agent's own
/// reply, and a supervisor turn opening on it, are none of the three, and an
/// absent field is what a consumer reads as unknown.
#[cfg(unix)]
#[test]
fn every_turn_of_a_conversation_names_who_authored_what_it_carries() {
    let _serial = serial();
    let workspace = Workspace::new();
    let began = workspace.at("worker-began");
    let release = workspace.at("worker-release");
    let task = format!(
        "fake:should-fail fake:entered={} fake:hold={}",
        began.display(),
        release.display()
    );
    let running = start(&workspace, "", &task);

    until("the worker's turn to be in flight", || began.exists());

    let text = "the release blocker is P0: fix it before anything else";
    let note = Note::new(Addressee::Worker, text)
        .expect("a note with text in it")
        .with_delivery(NoteDeliveryMode::TurnEnd);
    let events = running.run.started().events_path.clone();
    let offered = offering(&workspace, &running, note);
    wait_the_ordering_margin();
    std::fs::write(&release, "go").expect("release the worker's held turn");

    assert_eq!(
        offered.join().expect("the offering thread"),
        NoteDelivery::Accepted(Accepted::AtTurnEnd {
            party: Party::Worker
        }),
        "the note never reached the worker's turn once it ended"
    );
    running.run.wait().expect("the member settles");

    let published = std::fs::read_to_string(&events).expect("the run's own stream");
    let of_kind = |kind: &str| published_of_kind(&published, kind);

    let opened = of_kind("turn-started");
    assert!(!opened.is_empty(), "the run announced no turn: {published}");

    // The turn the member was opened on is the composed task.
    let first = opened
        .iter()
        .find(|event| event["payload"]["role"] == "assistant")
        .expect("the worker took a turn");
    assert_eq!(
        origin(first).as_deref(),
        Some("task"),
        "the opening turn was not stamped as the composed task: {first}"
    );
    assert!(
        first["payload"]["instruction"]
            .as_str()
            .is_some_and(|it| it.contains("fake:should-fail")),
        "the opening turn answered something other than the task: {first}"
    );

    // The turn that carries what the caller handed this graph is stamped as a
    // delivery — and it is the only one, so nothing else is attributed to an
    // operator who said it once.
    let delivered: Vec<&serde_json::Value> = opened
        .iter()
        .filter(|event| {
            event["payload"]["instruction"]
                .as_str()
                .is_some_and(|it| it.contains(text))
        })
        .collect();
    assert_eq!(
        delivered.len(),
        1,
        "the note reached no turn, or more than one: {opened:#?}"
    );
    assert_eq!(
        origin(delivered[0]).as_deref(),
        Some("delivered"),
        "the turn carrying the caller's own words was not stamped as a delivery: {}",
        delivered[0]
    );
    assert_eq!(
        opened
            .iter()
            .filter(|event| origin(event).as_deref() == Some("delivered"))
            .count(),
        1,
        "a turn nobody delivered into read as a delivery: {opened:#?}"
    );

    // The supervising side's own words, on the turn carrying them and on the
    // worker turn opened to answer them.
    let messages = of_kind("turn-message");
    let supervised: Vec<&serde_json::Value> = messages
        .iter()
        .filter(|event| event["payload"]["role"] == "user")
        .collect();
    assert!(
        !supervised.is_empty(),
        "the supervisor never spoke: {messages:#?}"
    );
    for event in &supervised {
        assert_eq!(
            origin(event).as_deref(),
            Some("supervisor"),
            "the supervising side's own words were not stamped as its own: {event}"
        );
    }
    assert!(
        opened.iter().any(|event| {
            origin(event).as_deref() == Some("supervisor")
                && event["payload"]["instruction"]
                    .as_str()
                    .is_some_and(|it| it.contains("verify it before you call it done"))
        }),
        "no worker turn opened on the supervisor's instruction was stamped as its own: {opened:#?}"
    );

    // And what this graph cannot attribute it does not: an agent's own reply, and
    // the supervisor turn that opens on it, carry no origin key at all — which a
    // consumer reads as unknown rather than as any of the three.
    for event in messages
        .iter()
        .filter(|event| event["payload"]["role"] == "assistant")
    {
        assert_eq!(
            event["payload"].get("origin"),
            None,
            "the agent's own words were attributed to somebody: {event}"
        );
    }
    let supervisor_turns: Vec<&serde_json::Value> = opened
        .iter()
        .filter(|event| event["payload"]["role"] == "user")
        .collect();
    assert!(
        !supervisor_turns.is_empty(),
        "the supervisor took no turn: {opened:#?}"
    );
    for event in supervisor_turns {
        assert_eq!(
            event["payload"].get("origin"),
            None,
            "a supervisor turn opening on the worker's own reply was attributed: {event}"
        );
    }
}

/// A turn is stamped as a delivery on the conversation's own record of having
/// handed it a note — never on the note's words turning up in an instruction.
///
/// The words are no evidence. A short, valid note can already occur in the task
/// the member opened on, and the supervisor's standing instruction is exactly
/// the kind of phrase an operator would send as a note; an implementation that
/// read the text as the delivery would stamp the task, or every later supervisor
/// turn, as the operator's — which is the misattribution this field exists to
/// end, in the other direction. So the note here **is** the supervisor's every
/// instruction, word for word, and it is in the task as well. Exactly one turn
/// is the delivery: the one the engine opened carrying it.
///
/// The unsuccessful half: a note the conversation refuses is never in its
/// record, so it can attribute nothing. The engine refuses a note only once the
/// conversation is over — there is no earlier refusal in its contract — so what
/// this drives is that a refused offer is reported as one on the stream and
/// leaves the set of delivery-stamped turns exactly as it was.
#[cfg(unix)]
#[test]
fn a_delivery_is_attributed_by_the_conversations_record_and_never_by_its_text() {
    let _serial = serial();
    let workspace = Workspace::new();
    let began = workspace.at("worker-began");
    let release = workspace.at("worker-release");
    // The phrase the fake supervisor answers every continuing turn with, and the
    // note's whole text — and part of the task, so the opening turn carries it
    // before any note exists.
    let text = "verify it before you call it done";
    let running = start(
        &workspace,
        "",
        &format!(
            "fake:should-fail fake:entered={} fake:hold={} {text}",
            began.display(),
            release.display()
        ),
    );

    until("the worker's turn to be in flight", || began.exists());

    // At the turn's end, so the engine opens a turn carrying it — the one turn
    // the record names.
    let note = Note::new(Addressee::Worker, text)
        .expect("a note with text in it")
        .with_delivery(NoteDeliveryMode::TurnEnd);
    let events = running.run.started().events_path.clone();
    let offered = offering(&workspace, &running, note.clone());
    wait_the_ordering_margin();
    std::fs::write(&release, "go").expect("release the worker's held turn");
    assert_eq!(
        offered.join().expect("the offering thread"),
        NoteDelivery::Accepted(Accepted::AtTurnEnd {
            party: Party::Worker
        }),
        "the note never reached the worker's turn once it ended"
    );
    running.run.wait().expect("the member settles");

    // A second offer, into a conversation that is over: refused to the caller,
    // and so never in the conversation's record.
    let refused = control::note(
        &workspace.state(),
        &running.id,
        &running.member,
        &note,
        &oneharness_bin(),
    )
    .expect("the run and its member are addressable");
    assert!(
        matches!(refused, NoteDelivery::Undelivered(_)),
        "a note into a settled conversation was not refused: {refused:?}"
    );

    let published = std::fs::read_to_string(&events).expect("the run's own stream");
    let opened = published_of_kind(&published, "turn-started");
    let worker_turns: Vec<&serde_json::Value> = opened
        .iter()
        .filter(|event| event["payload"]["role"] == "assistant")
        .collect();
    assert!(
        worker_turns.len() >= 3,
        "the run took too few worker turns to tell the three apart: {opened:#?}"
    );
    let instruction = |event: &serde_json::Value| -> String {
        event["payload"]["instruction"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    // Every worker turn's instruction carries the note's words — the task, the
    // delivery, and the supervisor's own instruction alike — so the text alone
    // cannot tell them apart, which is the point.
    for event in &worker_turns {
        assert!(
            instruction(event).contains(text),
            "a worker turn opened without the phrase, so this journey proves nothing: {event}"
        );
    }

    // The opening turn carried the phrase before any note existed: the task.
    assert_eq!(
        origin(worker_turns[0]).as_deref(),
        Some("task"),
        "the task was attributed to a note that did not yet exist: {}",
        worker_turns[0]
    );
    // The turn the engine opened carrying the note is the delivery — the one the
    // conversation's record names, and the only one.
    assert_eq!(
        origin(worker_turns[1]).as_deref(),
        Some("delivered"),
        "the turn that received the note was not stamped as a delivery: {}",
        worker_turns[1]
    );
    assert!(
        instruction(worker_turns[1]).contains("delivered to YOU, the worker"),
        "the delivery stamp landed on a turn that was not handed the note: {}",
        worker_turns[1]
    );
    // And every turn after it opens on the supervisor's own words, which happen
    // to be the note's — and are stamped as the supervisor's.
    for event in &worker_turns[2..] {
        assert_eq!(
            origin(event).as_deref(),
            Some("supervisor"),
            "a supervisor instruction that merely reads like the note was attributed to the \
             operator: {event}"
        );
    }
    assert_eq!(
        opened
            .iter()
            .filter(|event| origin(event).as_deref() == Some("delivered"))
            .count(),
        1,
        "more than one turn claimed the one delivery: {opened:#?}"
    );

    // The stream reports the one delivery the conversation took, and nothing
    // for the refused offer: a settled member's stream is closed, and that
    // refusal was answered from the run's own record to the caller alone.
    let reported = published_of_kind(&published, "turn-interrupted");
    let delivered: Vec<bool> = reported
        .iter()
        .map(|event| event["payload"]["delivered"] == true)
        .collect();
    assert_eq!(
        delivered,
        vec![true],
        "the stream did not report exactly the one delivery: {reported:#?}"
    );
}

/// How long a journey waits for an answer it expects, before failing rather than
/// hanging the suite on a wait that will never return.
#[cfg(unix)]
const ANSWER_BOUND: std::time::Duration = std::time::Duration::from_secs(120);

/// Offer `note` to the running member through the library, expecting it in.
#[cfg(unix)]
fn admitted(workspace: &Workspace, running: &Running, note: &Note) -> NoteOffer {
    match control::offer_note(
        &workspace.state(),
        &running.id,
        &running.member,
        note,
        &oneharness_bin(),
    )
    .expect("the run and its member are addressable")
    {
        NoteOffered::Admitted(offer) => offer,
        NoteOffered::Undelivered(undelivered) => {
            panic!("the note was not admitted: {undelivered}")
        }
    }
}

/// Wait for `offer`'s answer on a thread of its own, so the journey can hold or
/// release a turn while it waits.
#[cfg(unix)]
fn answering(offer: NoteOffer) -> std::sync::mpsc::Receiver<NoteDelivery> {
    let (answered, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = answered.send(offer.wait().expect("a wait answers"));
    });
    answer
}

/// Every envelope of `kind` the run has published so far that names `offer`.
fn naming(events: &str, kind: &str, offer: &str) -> Vec<serde_json::Value> {
    published_of_kind(&std::fs::read_to_string(events).unwrap_or_default(), kind)
        .into_iter()
        .filter(|event| event["payload"]["offer"] == offer)
        .collect()
}

/// Whether any turn of the run was ever handed `text`: on either side's recorded
/// prompts, or as the instruction a turn the run announced opened on.
fn ever_presented(events: &str, recorded: &[&std::path::Path], text: &str) -> bool {
    let announced = published_of_kind(
        &std::fs::read_to_string(events).unwrap_or_default(),
        "turn-started",
    );
    announced.iter().any(|event| {
        event["payload"]["instruction"]
            .as_str()
            .is_some_and(|it| it.contains(text))
    }) || recorded
        .iter()
        .any(|path| prompts(path).iter().any(|prompt| prompt.contains(text)))
}

/// An offered note is **admitted** first — in the member's conversation inbox,
/// untaken, and published on the run's stream before anything answers it — and
/// then interrupts the worker's live turn: the replacement turn opens with it,
/// exactly once, and the judge's next prompt holds the criterion it binds.
///
/// Admission is the signal a test or a view waits on instead of a guess, so it is
/// asserted the moment the offer returns: the `note-admitted` naming the offer is
/// already on the stream, and no answer about it is. And once the note has been
/// interrupted into a turn it is that turn's — taking it back is refused, and the
/// offer is still the caller's to wait on.
#[cfg(unix)]
#[test]
fn an_offered_note_is_admitted_before_any_answer_and_then_interrupts_the_worker_with_its_criterion()
{
    let _serial = serial();
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
    let events = running.run.started().events_path.clone();

    let text = "fake:complete-now the release blocker is P0: fix it before anything else";
    let criterion = "the release blocker is fixed and covered by a test";
    let note = Note::new(Addressee::Worker, text)
        .expect("a note with text in it")
        .binding(criterion)
        .expect("a criterion a judge can hold work to");
    let offer = admitted(&workspace, &running, &note);
    let id = offer.id().to_string();

    let admissions = naming(&events, "note-admitted", &id);
    assert_eq!(
        admissions.len(),
        1,
        "the admission was not on the run's stream as the offer returned"
    );
    assert_eq!(admissions[0]["payload"]["member"], "worker");
    assert_eq!(admissions[0]["payload"]["addressee"], "worker");
    assert!(
        naming(&events, "turn-interrupted", &id).is_empty(),
        "the note was answered on the stream before its admission was"
    );

    until(
        "the worker's replacement turn to open carrying the note",
        || {
            prompts(&agent_prompts)
                .iter()
                .any(|prompt| prompt.contains(text))
        },
    );
    let offer = offer
        .withdraw()
        .expect_err("a note already interrupted into a turn was taken back");
    let delivery = answering(offer)
        .recv_timeout(ANSWER_BOUND)
        .expect("the note was answered");
    assert_eq!(
        delivery,
        NoteDelivery::Accepted(Accepted::Interrupted {
            party: Party::Worker
        }),
        "a note offered into the worker's live turn did not interrupt it"
    );
    std::fs::write(&release, "go").expect("release the held turn, if anything still holds it");
    running.run.wait().expect("the member settles");

    let carrying: Vec<String> = prompts(&agent_prompts)
        .into_iter()
        .filter(|prompt| prompt.contains(text))
        .collect();
    assert_eq!(
        carrying.len(),
        1,
        "the note did not reach exactly one worker turn: {carrying:#?}"
    );
    assert_eq!(
        carrying[0].matches(text).count(),
        1,
        "the replacement turn was handed the note more than once: {}",
        carrying[0]
    );
    let judged = prompts(&judge_prompts);
    assert!(
        judged
            .first()
            .is_some_and(|prompt| prompt.contains(criterion)),
        "the judge's next prompt did not hold the criterion the note bound: {judged:#?}"
    );

    let answered = naming(&events, "turn-interrupted", &id);
    assert_eq!(answered.len(), 1, "the answer was not published once");
    let payload = &answered[0]["payload"];
    assert_eq!(payload["delivered"], true);
    assert_eq!(payload["accepted"], "interrupted");
    assert_eq!(payload["party"], "worker");
    assert!(
        answered[0]["seq"].as_u64() > admissions[0]["seq"].as_u64(),
        "the answer was published ahead of the admission"
    );
}

/// The judge side asks for a controllable turn of its own, so the member's
/// control record names where its **supervisor** turn is addressed, and a note to
/// the supervisor redirects the decision in flight — while a turn-end note sent
/// into that redirected decision waits for it to end. The two answers read apart
/// on the run's stream by `accepted`.
///
/// `should-fail` keeps the supervisor continuing, so the redirected decision is
/// one that sends the worker on — [`Accepted::Interrupted`] — rather than the
/// completion that would answer [`Accepted::JudgedWith`].
#[cfg(unix)]
#[test]
fn a_supervisor_note_redirects_the_live_decision_and_a_turn_end_one_waits_for_it_to_end() {
    let _serial = serial();
    let workspace = Workspace::new();
    let judge_prompts = workspace.at("judge-prompts");
    let gate = workspace.at("judge-gate");
    let entered = workspace.at("judge-gate.entered");
    let running = start(
        &workspace,
        "",
        &format!(
            "fake:record-supervisor-prompt={} fake:should-fail fake:supervisor-hold={}",
            judge_prompts.display(),
            gate.display()
        ),
    );
    until("the judge's own turn to be in flight", || entered.exists());
    let events = running.run.started().events_path.clone();

    let control_record = workspace
        .state()
        .join(running.id.as_str())
        .join("members")
        .join("worker")
        .join("control.json");
    let live: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&control_record).expect("the member's control record"),
    )
    .expect("the control record is JSON");
    assert_eq!(live["supervisor"]["state"], "open", "{live}");
    assert!(
        live["supervisor"]["address"]["session"]
            .as_str()
            .is_some_and(|session| session.ends_with("-user")),
        "the record does not address the supervisor's own session: {live}"
    );
    assert_ne!(
        live["supervisor"]["address"]["session"], live["turn"]["address"]["session"],
        "the two sides were given one address"
    );

    // The redirect: the held decision is interrupted, and its replacement holds
    // again at the same gate — which is the live turn the next note meets.
    std::fs::remove_file(&entered).expect("clear the judge's marker");
    let ruling = "hold the work to the amended bar: reversibility is now in scope";
    let redirecting = admitted(
        &workspace,
        &running,
        &Note::new(Addressee::Supervisor, ruling).expect("a note with text in it"),
    );
    let redirect_id = redirecting.id().to_string();
    let redirected = answering(redirecting);
    until("the redirected decision to be in flight", || {
        entered.exists()
    });
    assert!(
        prompts(&judge_prompts)
            .iter()
            .any(|prompt| prompt.contains(ruling)),
        "the redirected decision was not handed the note"
    );

    let follow_up = "and record the rollback plan in the pull request";
    let waiting = admitted(
        &workspace,
        &running,
        &Note::new(Addressee::Supervisor, follow_up)
            .expect("a note with text in it")
            .with_delivery(NoteDeliveryMode::TurnEnd),
    );
    let waiting_id = waiting.id().to_string();
    let at_end = answering(waiting);
    wait_the_ordering_margin();
    assert!(
        at_end.try_recv().is_err(),
        "a turn-end note was answered while the decision it waits for was still held"
    );
    std::fs::write(&gate, "go").expect("release the judge");

    assert_eq!(
        redirected
            .recv_timeout(ANSWER_BOUND)
            .expect("the redirect was answered"),
        NoteDelivery::Accepted(Accepted::Interrupted {
            party: Party::Supervisor
        }),
        "a note to the supervisor did not redirect its live decision"
    );
    assert_eq!(
        at_end
            .recv_timeout(ANSWER_BOUND)
            .expect("the turn-end note was answered"),
        NoteDelivery::Accepted(Accepted::AtTurnEnd {
            party: Party::Supervisor
        }),
        "a turn-end note did not reach the decision once it ended"
    );
    running.run.wait().expect("the member settles");

    let interrupted = naming(&events, "turn-interrupted", &redirect_id);
    let turn_end = naming(&events, "turn-interrupted", &waiting_id);
    assert_eq!((interrupted.len(), turn_end.len()), (1, 1));
    assert_eq!(interrupted[0]["payload"]["accepted"], "interrupted");
    assert_eq!(turn_end[0]["payload"]["accepted"], "at-turn-end");
    for answered in [&interrupted[0], &turn_end[0]] {
        assert_eq!(answered["payload"]["delivered"], true);
        assert_eq!(answered["payload"]["party"], "supervisor");
    }

    // The drift gate: the address written while the member ran is the one the
    // report says its supervisor turns were bound under.
    let settled: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&control_record).expect("the member's control record"),
    )
    .expect("the control record is JSON");
    assert_eq!(
        settled["supervisor"]["address"]["session"], live["supervisor"]["address"]["session"],
        "{settled}"
    );
}

/// A turn-end note offered during the worker's held turn waits for that turn to
/// end: unanswered while it is held, then [`Accepted::AtTurnEnd`] — and the run's
/// stream says it reached the worker at the turn's end, not that the turn was
/// interrupted.
#[cfg(unix)]
#[test]
fn a_turn_end_note_is_answered_only_once_the_held_turn_ends() {
    let _serial = serial();
    let workspace = Workspace::new();
    let agent_prompts = workspace.at("agent-prompts");
    let began = workspace.at("worker-began");
    let release = workspace.at("worker-release");
    let running = start(
        &workspace,
        &format!("fake:record-prompt={}", agent_prompts.display()),
        &format!(
            "fake:should-fail fake:entered={} fake:hold={}",
            began.display(),
            release.display()
        ),
    );
    until("the worker's turn to be in flight", || began.exists());
    let events = running.run.started().events_path.clone();

    let text = "once this turn is done, rebase onto main";
    let offer = admitted(
        &workspace,
        &running,
        &Note::new(Addressee::Worker, text)
            .expect("a note with text in it")
            .with_delivery(NoteDeliveryMode::TurnEnd),
    );
    let id = offer.id().to_string();
    let answer = answering(offer);
    wait_the_ordering_margin();
    assert!(
        answer.try_recv().is_err(),
        "a turn-end note was answered while the turn it waits for was still held"
    );
    assert!(
        !prompts(&agent_prompts)
            .iter()
            .any(|prompt| prompt.contains(text)),
        "a turn-end note reached the worker while its turn was still held"
    );

    std::fs::write(&release, "go").expect("release the worker's held turn");
    assert_eq!(
        answer
            .recv_timeout(ANSWER_BOUND)
            .expect("the note was answered"),
        NoteDelivery::Accepted(Accepted::AtTurnEnd {
            party: Party::Worker
        }),
    );
    running.run.wait().expect("the member settles");

    assert!(
        prompts(&agent_prompts)
            .iter()
            .any(|prompt| prompt.contains(text)),
        "the note never reached a worker turn"
    );
    let answered = naming(&events, "turn-interrupted", &id);
    assert_eq!(answered.len(), 1, "the answer was not published once");
    assert_eq!(answered[0]["payload"]["delivered"], true);
    assert_eq!(answered[0]["payload"]["accepted"], "at-turn-end");
    assert_eq!(answered[0]["payload"]["party"], "worker");
}

/// A turn-end note whose deadline passes while the turn is still held **expires**:
/// [`NoteDelivery::Expired`], a `note-withdrawn` naming it with reason `expired`,
/// and no turn ever presented with it.
#[cfg(unix)]
#[test]
fn a_note_whose_deadline_passes_while_the_turn_is_held_expires_and_is_never_presented() {
    let _serial = serial();
    let workspace = Workspace::new();
    let agent_prompts = workspace.at("agent-prompts");
    let judge_prompts = workspace.at("judge-prompts");
    let began = workspace.at("worker-began");
    let release = workspace.at("worker-release");
    let running = start(
        &workspace,
        &format!("fake:record-prompt={}", agent_prompts.display()),
        &format!(
            "fake:record-supervisor-prompt={} fake:should-fail fake:entered={} fake:hold={}",
            judge_prompts.display(),
            began.display(),
            release.display()
        ),
    );
    until("the worker's turn to be in flight", || began.exists());
    let events = running.run.started().events_path.clone();

    let text = "this correction was only worth making in the next minute";
    let offer = admitted(
        &workspace,
        &running,
        &Note::new(Addressee::Worker, text)
            .expect("a note with text in it")
            .with_delivery(NoteDeliveryMode::TurnEnd),
    );
    let id = offer.id().to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    assert_eq!(
        offer.wait_until(deadline).expect("a wait answers"),
        NoteDelivery::Expired,
        "a note untaken at its deadline did not expire"
    );
    assert!(
        !release.exists(),
        "the turn was released before the deadline, so this proves nothing"
    );
    let withdrawn = naming(&events, "note-withdrawn", &id);
    assert_eq!(withdrawn.len(), 1, "the expiry was not published once");
    assert_eq!(withdrawn[0]["payload"]["reason"], "expired");
    assert_eq!(withdrawn[0]["payload"]["member"], "worker");

    std::fs::write(&release, "go").expect("release the worker's held turn");
    running.run.wait().expect("the member settles");
    assert!(
        !ever_presented(&events, &[&agent_prompts, &judge_prompts], text),
        "an expired note was presented to a turn"
    );
    assert!(
        naming(&events, "turn-interrupted", &id).is_empty(),
        "an expired note was also answered as delivered"
    );
}

/// A note withdrawn before any turn takes it is taken back: `withdraw` answers
/// `Ok`, the run's stream says so exactly once — naming the member and the same
/// offer id its `note-admitted` carried — and no turn is ever presented with it.
#[cfg(unix)]
#[test]
fn a_withdrawn_note_is_taken_back_once_and_never_presented() {
    let _serial = serial();
    let workspace = Workspace::new();
    let agent_prompts = workspace.at("agent-prompts");
    let judge_prompts = workspace.at("judge-prompts");
    let began = workspace.at("worker-began");
    let release = workspace.at("worker-release");
    let running = start(
        &workspace,
        &format!("fake:record-prompt={}", agent_prompts.display()),
        &format!(
            "fake:record-supervisor-prompt={} fake:should-fail fake:entered={} fake:hold={}",
            judge_prompts.display(),
            began.display(),
            release.display()
        ),
    );
    until("the worker's turn to be in flight", || began.exists());
    let events = running.run.started().events_path.clone();

    let text = "never mind the migration, it was already reversible";
    let offer = admitted(
        &workspace,
        &running,
        &Note::new(Addressee::Worker, text)
            .expect("a note with text in it")
            .with_delivery(NoteDeliveryMode::TurnEnd),
    );
    let id = offer.id().to_string();
    // Past the instant the live turn's watch reads a turn-end note and hands it
    // back — onejudge's documented window in which a withdrawal reads as taken.
    std::thread::sleep(std::time::Duration::from_millis(500));
    offer
        .withdraw()
        .expect("a note no turn had taken was not taken back");

    std::fs::write(&release, "go").expect("release the worker's held turn");
    running.run.wait().expect("the member settles");

    let published = std::fs::read_to_string(&events).expect("the run's own stream");
    let withdrawn = published_of_kind(&published, "note-withdrawn");
    assert_eq!(withdrawn.len(), 1, "the withdrawal was not published once");
    assert_eq!(withdrawn[0]["payload"]["reason"], "withdrawn");
    assert_eq!(withdrawn[0]["payload"]["member"], "worker");
    assert_eq!(withdrawn[0]["payload"]["offer"], id.as_str());
    let admissions = published_of_kind(&published, "note-admitted");
    assert_eq!(admissions.len(), 1);
    assert_eq!(
        admissions[0]["payload"]["offer"], withdrawn[0]["payload"]["offer"],
        "the withdrawal names a different offer from the admission"
    );
    assert!(
        !ever_presented(&events, &[&agent_prompts, &judge_prompts], text),
        "a withdrawn note was presented to a turn"
    );
}

/// A default note — one that would interrupt — offered to a held turn that
/// **cannot** be interrupted, because its harness offers no control, waits for the
/// turn's end; given a deadline that passes first, it expires before the turn is
/// released, and nothing presents it afterwards.
#[cfg(unix)]
#[test]
fn an_interrupting_note_to_a_turn_with_no_lever_expires_at_its_deadline_and_is_never_presented() {
    let _serial = serial();
    let workspace = Workspace::new();
    workspace.write("oneharness.toml", QWEN_CHAIN);
    workspace.write("oneharness.judge.toml", QWEN_CHAIN);
    let recorded = workspace.at("prompts");
    let began = workspace.at("worker-began");
    let release = workspace.at("worker-release");
    workspace.write(
        "base.yaml",
        &base_with(&format!("fake:record-prompt={}", recorded.display())),
    );
    workspace.graph(&two_party_graph(
        &crate::support::fake_harness(),
        &[
            ("ONEHARNESS_BIN_QWEN", crate::support::fake_harness()),
            (
                "XDG_STATE_HOME",
                workspace.session_store().display().to_string(),
            ),
        ],
    ));
    let mut env = BTreeMap::new();
    env.insert(
        "XDG_STATE_HOME".to_string(),
        workspace.session_store().display().to_string(),
    );
    let request = Request {
        graph: ConfigRef(workspace.at("graph.yaml").display().to_string()),
        task: Some(format!(
            "fake:record-prompt={} fake:should-fail fake:entered={} fake:hold={}",
            recorded.display(),
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
    let running = Running {
        id: run.started().run_id.clone(),
        run,
        member: MemberName::parse("worker").expect("a member name"),
    };
    until("the worker's turn to be in flight", || began.exists());
    let events = running.run.started().events_path.clone();

    let text = "stop and summarise what you have so far";
    let offer = admitted(
        &workspace,
        &running,
        &Note::new(Addressee::Worker, text).expect("a note with text in it"),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    assert_eq!(
        offer.wait_until(deadline).expect("a wait answers"),
        NoteDelivery::Expired,
        "a note no lever could deliver did not expire at its deadline"
    );
    assert!(
        !release.exists(),
        "the turn was released before the deadline, so this proves nothing"
    );

    std::fs::write(&release, "go").expect("release the worker's held turn");
    running.run.wait().expect("the member settles");
    assert!(
        !ever_presented(&events, &[&recorded], text),
        "an expired note was presented once the turn it waited for ended"
    );
}

/// A caller that waits only after the member has settled still reads what became
/// of its note: the member answered every offer it held as it ended and wrote
/// each answer down, so the wait is answered from there rather than refused by a
/// spool nothing serves any more.
#[cfg(unix)]
#[test]
fn a_caller_that_waits_after_the_member_settled_reads_what_became_of_its_note() {
    let _serial = serial();
    let workspace = Workspace::new();
    let began = workspace.at("worker-began");
    let release = workspace.at("worker-release");
    let running = start(
        &workspace,
        "",
        &format!(
            "fake:should-fail fake:entered={} fake:hold={}",
            began.display(),
            release.display()
        ),
    );
    until("the worker's turn to be in flight", || began.exists());

    let offer = admitted(
        &workspace,
        &running,
        &Note::new(
            Addressee::Worker,
            "once this turn is done, rebase onto main",
        )
        .expect("a note with text in it")
        .with_delivery(NoteDeliveryMode::TurnEnd),
    );
    std::fs::write(&release, "go").expect("release the worker's held turn");
    running.run.wait().expect("the member settles");

    assert_eq!(
        offer.wait().expect("a wait answers"),
        NoteDelivery::Accepted(Accepted::AtTurnEnd {
            party: Party::Worker
        }),
        "a caller asking after the member settled was not told what became of its note"
    );
}

/// A wait bounded by a deadline returns the conversation's answer whenever the
/// note was taken before that deadline — however late the answer comes. A note
/// to the supervisor is taken the moment it redirects the judge's live decision,
/// but answered only once that decision is taken, which the journey holds past
/// the deadline.
#[cfg(unix)]
#[test]
fn a_note_taken_before_its_deadline_is_answered_however_late_the_answer_comes() {
    let _serial = serial();
    let workspace = Workspace::new();
    let gate = workspace.at("judge-gate");
    let entered = workspace.at("judge-gate.entered");
    let running = start(
        &workspace,
        "",
        &format!("fake:should-fail fake:supervisor-hold={}", gate.display()),
    );
    until("the judge's own turn to be in flight", || entered.exists());
    std::fs::remove_file(&entered).expect("clear the judge's marker");

    let offer = admitted(
        &workspace,
        &running,
        &Note::new(
            Addressee::Supervisor,
            "the bar now includes a rollback plan",
        )
        .expect("a note with text in it"),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let (answered, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = answered.send(offer.wait_until(deadline).expect("a wait answers"));
    });
    until("the redirected decision to be in flight", || {
        entered.exists()
    });
    std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
    wait_the_ordering_margin();
    assert!(
        answer.try_recv().is_err(),
        "the decision the note redirected was answered before it was taken"
    );
    std::fs::write(&gate, "go").expect("release the judge");
    assert_eq!(
        answer
            .recv_timeout(ANSWER_BOUND)
            .expect("the note was answered"),
        NoteDelivery::Accepted(Accepted::Interrupted {
            party: Party::Supervisor
        }),
        "a note taken before its deadline was not answered once the decision came back"
    );
    running.run.wait().expect("the member settles");
}

/// A member whose conversation ends with notes no turn took answers every one of
/// them as undelivered — a wait already in flight, bounded by a deadline that has
/// not passed, as the conversation ends, and a caller that waits only afterwards
/// from the answer the member wrote down — and publishes each on its stream.
///
/// The member is cancelled while both notes wait for its held turn to end, which
/// ends the conversation with neither taken.
#[cfg(unix)]
#[test]
fn a_member_that_ends_with_notes_untaken_answers_each_as_undelivered() {
    let _serial = serial();
    let workspace = Workspace::new();
    let began = workspace.at("worker-began");
    let release = workspace.at("worker-release");
    let running = start(
        &workspace,
        "",
        &format!(
            "fake:should-fail fake:entered={} fake:hold={}",
            began.display(),
            release.display()
        ),
    );
    until("the worker's turn to be in flight", || began.exists());
    let events = running.run.started().events_path.clone();
    let turn_end = |text: &str| {
        Note::new(Addressee::Worker, text)
            .expect("a note with text in it")
            .with_delivery(NoteDeliveryMode::TurnEnd)
    };
    let waited = admitted(
        &workspace,
        &running,
        &turn_end("first, the open review comments"),
    );
    let late = admitted(&workspace, &running, &turn_end("then, the changelog"));
    let (waited_id, late_id) = (waited.id().to_string(), late.id().to_string());
    let deadline = std::time::Instant::now() + ANSWER_BOUND;
    let (answered, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = answered.send(waited.wait_until(deadline).expect("a wait answers"));
    });
    wait_the_ordering_margin();

    running.run.cancel().expect("the run takes the cancel");
    std::fs::write(&release, "go").expect("release the worker's held turn");
    let _ = running.run.wait();

    assert!(
        matches!(
            answer
                .recv_timeout(ANSWER_BOUND)
                .expect("the in-flight wait was answered"),
            NoteDelivery::Undelivered(_)
        ),
        "a wait in flight as the conversation ended was not told its note was undelivered"
    );
    assert!(
        matches!(
            late.wait().expect("a wait answers"),
            NoteDelivery::Undelivered(_)
        ),
        "a caller waiting after the member ended was not told its note was undelivered"
    );
    for id in [&waited_id, &late_id] {
        let answered = naming(&events, "turn-interrupted", id);
        assert_eq!(
            answered.len(),
            1,
            "{id} was not answered on the stream once"
        );
        assert_eq!(answered[0]["payload"]["delivered"], false);
        assert!(
            answered[0]["payload"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("was not delivered")),
            "{}",
            answered[0]
        );
        assert!(answered[0]["payload"].get("accepted").is_none());
    }
}

/// `--output text` renders a note's life as a person watching the run reads it:
/// its admission, naming the offer and whom it is for, and its answer, saying how
/// the note was accepted.
#[cfg(unix)]
#[test]
fn a_text_rendering_shows_a_note_admitted_and_how_it_was_accepted() {
    use crate::support::Hermetic;

    let workspace = Workspace::new();
    let began = workspace.at("worker-began");
    let release = workspace.at("worker-release");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_oneagentgraph"))
        .args([
            "run",
            "./graph.yaml",
            "--output",
            "text",
            "--task",
            &format!(
                "fake:entered={} fake:hold={}",
                began.display(),
                release.display()
            ),
            "--dir",
            &workspace.dir().display().to_string(),
        ])
        .current_dir(workspace.path())
        .env("ONEAGENTGRAPH_STATE_DIR", workspace.state())
        .env("ONEAGENTGRAPH_ONEHARNESS_BIN", oneharness_bin())
        .env("XDG_STATE_HOME", workspace.session_store())
        .hermetic()
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the run starts");
    until("the worker's turn to be in flight", || began.exists());
    let run_id = run::RunId::parse(
        workspace.record()["run_id"]
            .as_str()
            .expect("the run recorded its id"),
    )
    .expect("a run id");
    let member = MemberName::parse("worker").expect("a member name");
    let offered = control::offer_note(
        &workspace.state(),
        &run_id,
        &member,
        &Note::new(Addressee::Worker, "fake:complete-now ship the fix first")
            .expect("a note with text in it"),
        &oneharness_bin(),
    )
    .expect("the run and its member are addressable");
    let NoteOffered::Admitted(offer) = offered else {
        panic!("the note was not admitted: {offered:?}");
    };
    let id = offer.id().to_string();
    assert_eq!(
        offer.wait().expect("a wait answers"),
        NoteDelivery::Accepted(Accepted::Interrupted {
            party: Party::Worker
        })
    );
    std::fs::write(&release, "go").expect("release the worker's held turn");
    let output = child.wait_with_output().expect("the run finishes");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout
            .lines()
            .any(|line| line.contains(&format!("note-admitted {id} for worker"))),
        "the admission was not rendered:\n{stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.contains("turn-interrupted delivered interrupted (worker)")),
        "how the note was accepted was not rendered:\n{stdout}"
    );
}
