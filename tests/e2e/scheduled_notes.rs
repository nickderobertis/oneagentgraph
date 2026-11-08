//! Scheduled notes, `reset_on`, and a followed event stream, against the
//! compiled binary and real onejudge turns.
//!
//! A two-party member's scheduled note is offered into its conversation on a
//! clock of its own, and each journey here places one firing at one of the
//! timings the contract lists — during a hold, during a live turn, at the
//! instant a hold expires, twice within one long turn, after a reset, after the
//! conversation ended, before it opened — and reads off the run's own stream
//! exactly how many copies were admitted, accepted and withdrawn, and which
//! turn took the one that was delivered. Nothing is placed by sleeping: a turn
//! is held open with the harness double's `fake:hold`, a boundary is paused with
//! a fixture, and a note's clock starts counting only when the journey releases
//! `oneagentgraph-fixture:note-clock-from`.
//!
//! The `reset_on` journeys drive `--follow-events` on a file the journey appends
//! envelopes to, the way another program's stream grows beside a run.

// llmlint: ignore-file[e2e_not_mocked] the same declaration its sibling journey
// files carry, and for the same reason — see tests/e2e/support.rs: the paid
// harness process is the one sanctioned double, replaced at oneharness's own
// `ONEHARNESS_BIN_<ID>` seam. Real oneagentgraph runs every clock, real onejudge
// takes every note into the conversation it drives, and real oneharness starts
// the double for each turn. The command judge is an input the graph names, on
// `tests/e2e/two_party.rs`'s terms.

// llmlint: ignore-file[expensive_tests_stay_behind_their_own_edge] this crate has
// exactly one e2e target — `[[test]] name = "e2e"` in Cargo.toml — and every
// journey file is a module of it, by the design AGENTS.md states: a journey runs
// inside `just check` rather than behind `#[ignore]`. There is no narrower edge
// for these to sit behind. Each journey is one short run of the doubled
// harness, held only as long as the journey takes to release its gate.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;
use std::process::Child;
use std::time::Duration;

use oneagentgraph::run::{MemberName, RunId, Signal};
use serde_json::{json, Value};

use crate::support::{
    fake_harness, fake_provider, graph_with, until, Workspace, BASE, FAKE_HARNESS_KEY,
};
use crate::two_party::{
    base_without_done_when, exit_of, gap, of, precedes, run_id, stream, turns_of, until_held,
    FAKE_JUDGE, KEEP_GOING,
};

/// The id every journey's note is declared under.
const NOTE: &str = "nudge";

/// What the note says — distinctive, so a turn's prompt carrying it is told
/// apart from one that does not.
const NOTE_TEXT: &str = "SCHEDULED NUDGE: say where the work stands";

/// The fixture that starts a member's note clocks only once its gate exists.
const CLOCK_FROM: &str = "oneagentgraph-fixture:note-clock-from=";

/// The fixture that pauses a hold at the instant its `every` elapses.
const AT_EXPIRY: &str = "oneagentgraph-fixture:hold-at-expiry=";

/// The filter every `reset_on` journey declares.
const SURFACED: &str = "{include: [{source: pipeline, kind: planner-surfaced}]}";

/// One scheduled note, as a `scheduled_notes` list entry.
fn note_entry(id: &str, every: u64, start_after: u64, reset_on: Option<&str>) -> String {
    let reset_on = reset_on.map_or(String::new(), |filter| format!(", reset_on: {filter}"));
    format!(
        "      - {{id: {id}, every: {every}, start_after: {start_after}, addressee: worker, \
         text: '{NOTE_TEXT}'{reset_on}}}\n"
    )
}

/// A schema-11 graph whose `worker` is a conversation on the command judge,
/// declaring `notes`, paced by `schedule` when it names one, plus `extra`
/// members.
fn graph(schedule: Option<&str>, max_turns: u32, notes: &str, extra: &str) -> String {
    let schedule = schedule.map_or(String::new(), |schedule| {
        format!("    schedule: {schedule}\n    background: false\n")
    });
    let skeleton = format!(
        concat!(
            "version: 11\nname: notes\n",
            "env: {{}}\n",
            "members:\n",
            "  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      command: [placeholder]\n",
            "    mode: bypass\n",
            "    max_turns: {max_turns}\n",
            "{schedule}    scheduled_notes:\n{notes}{extra}",
        ),
        max_turns = max_turns,
        schedule = schedule,
        notes = notes,
        extra = extra,
    );
    graph_with(
        &skeleton,
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            (&format!("members.worker.{FAKE_JUDGE}"), fake_provider()),
        ],
    )
}

/// A single-sided member beside the worker, on the doubled harness.
fn single(name: &str, schedule: &str) -> String {
    format!(
        concat!(
            "  {name}:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    task: write one status update\n",
            "    schedule: {schedule}\n",
        ),
        name = name,
        schedule = schedule,
    )
}

/// Start the run of this workspace's graph on `task`, following `follow` when
/// it names a stream.
fn launch(workspace: &Workspace, task: &str, follow: Option<&Path>) -> Child {
    launch_with(workspace, task, follow, &[])
}

/// [`launch`], with `extra` arguments after the rest.
fn launch_with(workspace: &Workspace, task: &str, follow: Option<&Path>, extra: &[&str]) -> Child {
    let mut args = vec![
        "run".to_string(),
        "./graph.yaml".to_string(),
        "--task".to_string(),
        task.to_string(),
        "--dir".to_string(),
        workspace.dir().display().to_string(),
    ];
    if let Some(path) = follow {
        args.push("--follow-events".to_string());
        args.push(path.display().to_string());
    }
    args.extend(extra.iter().map(|arg| (*arg).to_string()));
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    workspace.spawn_with(&borrowed, &[])
}

// llmlint: ignore-block[tests_mirror_real_usage] the task names a
// `test-doubles` fixture that starts a member's note clocks when the journey
// releases a file, because the timings this file proves — a firing during a hold,
// during a live turn, twice within one turn — are wall-clock races against turns
// that take as long as the host makes them. The task asks for exactly this:
// each case held "with a file the test releases rather than by sleeping". The
// clock, the seam it offers through, onejudge and the binary are all the real
// ones; the fixture only chooses *when* counting begins, on the lever every
// fixture of this crate uses (`src/AGENTS.md`). The production wave-start is
// what every journey without `clocked` drives, the fixed-cadence one among them.
/// `task`, with this workspace's note clocks started by `gate`.
fn clocked(task: &str, gate: &Path) -> String {
    format!("{task} {CLOCK_FROM}{}", gate.display())
}
// llmlint: ignore-end[tests_mirror_real_usage]

fn release(gate: &Path) {
    std::fs::write(gate, "go").expect("a gate is writable");
}

/// One envelope of another program's stream.
fn envelope(seq: u64, source: &str, kind: &str) -> String {
    json!({
        "v": 1,
        "ts": "2026-11-08T00:00:00.000Z",
        "stream": "pipeline-run-1",
        "seq": seq,
        "source": source,
        "kind": kind,
        "labels": {"run_id": "P1", "node": "service", "step": "implement",
                   "member": "worker", "persona": "engineer"},
        "payload": {}
    })
    .to_string()
}

fn append(path: &Path, line: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("the followed stream is writable");
    writeln!(file, "{line}").expect("appended");
}

/// What became of one scheduled note's copies, read off the stream.
struct Ledger {
    /// Every `note-admitted` naming the note, in stream order.
    admitted: Vec<Value>,
    /// Every accepted delivery of one of those copies.
    accepted: Vec<Value>,
    /// Every `note-withdrawn` naming the note.
    withdrawn: Vec<Value>,
}

impl Ledger {
    fn of(events: &[Value], id: &str) -> Self {
        let admitted: Vec<Value> = of(events, "note-admitted", "worker")
            .into_iter()
            .filter(|event| event["payload"]["scheduled_note"] == id)
            .cloned()
            .collect();
        let offers: BTreeSet<&str> = admitted
            .iter()
            .filter_map(|event| event["payload"]["offer"].as_str())
            .collect();
        let accepted = of(events, "turn-interrupted", "worker")
            .into_iter()
            .filter(|event| {
                event["payload"]["delivered"] == true
                    && event["payload"]["offer"]
                        .as_str()
                        .is_some_and(|offer| offers.contains(offer))
            })
            .cloned()
            .collect();
        let withdrawn = of(events, "note-withdrawn", "worker")
            .into_iter()
            .filter(|event| event["payload"]["scheduled_note"] == id)
            .cloned()
            .collect();
        Self {
            admitted,
            accepted,
            withdrawn,
        }
    }

    fn offer(&self, index: usize) -> &str {
        self.admitted[index]["payload"]["offer"]
            .as_str()
            .expect("an admitted copy names its offer")
    }

    fn accepted_for(&self, offer: &str) -> Vec<&Value> {
        self.accepted
            .iter()
            .filter(|event| event["payload"]["offer"] == offer)
            .collect()
    }

    fn withdrawn_for(&self, offer: &str) -> Vec<&Value> {
        self.withdrawn
            .iter()
            .filter(|event| event["payload"]["offer"] == offer)
            .collect()
    }
}

/// Every firing of the scheduled note `id`'s clock.
fn note_fired<'a>(events: &'a [Value], id: &str) -> Vec<&'a Value> {
    of(events, "cron-fired", "worker")
        .into_iter()
        .filter(|event| event["payload"]["scheduled_note"] == id)
        .collect()
}

/// Every firing of `member`'s own schedule — a hold ending, or a single-sided
/// member's clock — which names no scheduled note.
fn schedule_fired<'a>(events: &'a [Value], member: &str) -> Vec<&'a Value> {
    of(events, "cron-fired", member)
        .into_iter()
        .filter(|event| event["payload"].get("scheduled_note").is_none())
        .collect()
}

/// The worker turns the stream says carried a delivered note, by number.
fn delivered_turns(events: &[Value]) -> Vec<u64> {
    turns_of(events, "worker", "assistant", "turn-started")
        .into_iter()
        .filter(|turn| turn["payload"]["origin"] == "delivered")
        .filter_map(|turn| turn["payload"]["turn"].as_u64())
        .collect()
}

/// Where `event` sits in `events`.
fn at(events: &[Value], event: &Value) -> usize {
    events
        .iter()
        .position(|candidate| candidate == event)
        .expect("the event is on the stream")
}

/// Whether the stream carries a `note-admitted` for `id` yet.
fn admitted_yet(workspace: &Workspace, id: &str, count: usize) -> bool {
    Ledger::of(&stream(workspace), id).admitted.len() >= count
}

/// (1) A firing during a hold ends the hold, and the note is delivered
/// `queued` into the next turn — admitted on the stream before the `cron-fired`
/// that ends the hold, and before that turn opens.
#[test]
fn a_note_firing_during_a_hold_ends_it_and_is_delivered_queued_into_the_next_turn() {
    let workspace = Workspace::new();
    let prompts = workspace.at("agent-prompts");
    workspace.write(
        "base.yaml",
        &base_without_done_when(&format!("fake:record-prompt={}", prompts.display())),
    );
    workspace.graph(&graph(
        Some("{every: 120, start_after: 0}"),
        2,
        &note_entry(NOTE, 3600, 0, None),
        "",
    ));
    let gate = workspace.at("note-clock");
    let child = launch(&workspace, &clocked(KEEP_GOING, &gate), None);
    until_held(&workspace, 1);
    release(&gate);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");

    let events = stream(&workspace);
    let ledger = Ledger::of(&events, NOTE);
    assert_eq!(ledger.admitted.len(), 1, "{events:#?}");
    assert_eq!(ledger.accepted.len(), 1, "{events:#?}");
    assert_eq!(ledger.accepted[0]["payload"]["accepted"], "queued");
    assert!(ledger.withdrawn.is_empty(), "{events:#?}");
    assert_eq!(delivered_turns(&events), vec![2], "{events:#?}");
    assert_eq!(note_fired(&events, NOTE).len(), 1);

    let agent = turns_of(&events, "worker", "assistant", "turn-started");
    let ended_hold = schedule_fired(&events, "worker");
    assert_eq!(
        ended_hold.len(),
        1,
        "one hold, ended by the note: {events:#?}"
    );
    assert!(
        precedes(&events, &ledger.admitted[0], ended_hold[0])
            && precedes(&events, ended_hold[0], agent[1]),
        "the note was not admitted before the hold it ended woke the member: {events:#?}"
    );
    let closed = turns_of(&events, "worker", "user", "turn-completed")[0];
    assert!(
        gap(closed, agent[1]) < 60_000,
        "the note did not end a 120-second hold"
    );
    let handed = std::fs::read_to_string(&prompts).unwrap_or_default();
    assert_eq!(handed.matches(NOTE_TEXT).count(), 1, "{handed}");
}

/// (2) A firing during a live turn interrupts nothing: it is delivered once that
/// turn ends, and the hold that would have followed is skipped — the next turn
/// opens with the note, with no supervisor turn and no hold between.
#[test]
fn a_note_firing_during_a_live_turn_waits_for_its_end_and_skips_the_next_hold() {
    let workspace = Workspace::new();
    let (entered, hold) = (workspace.at("turn-entered"), workspace.at("turn-hold"));
    workspace.write(
        "base.yaml",
        &base_without_done_when(&format!(
            "fake:entered={} fake:hold={}",
            entered.display(),
            hold.display()
        )),
    );
    workspace.graph(&graph(
        Some("{every: 120, start_after: 0}"),
        2,
        &note_entry(NOTE, 3600, 0, None),
        "",
    ));
    let gate = workspace.at("note-clock");
    let child = launch(&workspace, &clocked(KEEP_GOING, &gate), None);
    until("the first worker turn to be live", || entered.exists());
    release(&gate);
    until("the note to be admitted during the live turn", || {
        admitted_yet(&workspace, NOTE, 1)
    });
    release(&hold);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");

    let events = stream(&workspace);
    let ledger = Ledger::of(&events, NOTE);
    assert_eq!(ledger.admitted.len(), 1, "{events:#?}");
    assert_eq!(ledger.accepted.len(), 1, "{events:#?}");
    let accepted = &ledger.accepted[0]["payload"]["accepted"];
    assert!(
        accepted == "at-turn-end" || accepted == "queued",
        "the note was taken {accepted}, not at the live turn's end"
    );
    assert!(ledger.withdrawn.is_empty());
    assert!(
        of(&events, "turn-interrupted", "worker")
            .iter()
            .all(|event| event["payload"]["accepted"] != "interrupted"),
        "a scheduled note interrupted a live turn: {events:#?}"
    );

    let completed = turns_of(&events, "worker", "assistant", "turn-completed");
    let started = turns_of(&events, "worker", "assistant", "turn-started");
    assert!(
        at(&events, completed[0]) < at(&events, &ledger.accepted[0]),
        "the note was delivered before the live turn ended"
    );
    assert_eq!(delivered_turns(&events), vec![2], "{events:#?}");
    let between = &events[at(&events, completed[0])..at(&events, started[1])];
    assert!(
        between
            .iter()
            .all(|event| event["kind"] != "cron-fired" && event["payload"]["role"] != "user"),
        "a hold or a supervisor turn came between the live turn and the one the note opened: \
         {between:#?}"
    );
    assert!(
        gap(completed[0], started[1]) < 60_000,
        "the hold was not skipped"
    );
}

/// (3) A firing at the instant a hold expires is delivered once. The fixture
/// holds the run at the expiry boundary until the note's admission has been read
/// off the stream, so a run that woke the member before admitting the note
/// would fail here even if the note were delivered later.
#[test]
fn a_note_firing_at_the_instant_a_hold_expires_is_delivered_once() {
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    workspace.graph(&graph(
        Some("{every: 1, start_after: 0}"),
        2,
        &note_entry(NOTE, 3600, 0, None),
        "",
    ));
    let (gate, expiry) = (workspace.at("note-clock"), workspace.at("hold-expiry"));
    // llmlint: ignore-block[tests_mirror_real_usage] the instant a hold expires
    // is the boundary the task asks to be made deterministic "by a fixture that
    // pauses at that boundary"; the fixture is `test-doubles` only, pauses the
    // real hold after it decided to wake, and changes nothing a run does.
    let task = format!(
        "{} {AT_EXPIRY}{}",
        clocked(KEEP_GOING, &gate),
        expiry.display()
    );
    // llmlint: ignore-end[tests_mirror_real_usage]
    let child = launch(&workspace, &task, None);
    let paused = workspace.at("hold-expiry.entered");
    until("the hold to reach its expiry", || paused.exists());
    release(&gate);
    until("the note to be admitted at the expiry", || {
        admitted_yet(&workspace, NOTE, 1)
    });
    // Still at the boundary: the hold has not woken the member.
    let before = stream(&workspace);
    assert!(
        schedule_fired(&before, "worker").is_empty()
            && turns_of(&before, "worker", "assistant", "turn-started").len() == 1,
        "the member woke while the hold was paused at its expiry: {before:#?}"
    );
    release(&expiry);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");

    let events = stream(&workspace);
    let ledger = Ledger::of(&events, NOTE);
    assert_eq!(ledger.admitted.len(), 1, "{events:#?}");
    assert_eq!(ledger.accepted.len(), 1, "{events:#?}");
    assert!(ledger.withdrawn.is_empty());
    assert_eq!(delivered_turns(&events), vec![2], "{events:#?}");
    let ended_hold = schedule_fired(&events, "worker");
    let agent = turns_of(&events, "worker", "assistant", "turn-started");
    assert_eq!(ended_hold.len(), 1, "{events:#?}");
    assert!(
        precedes(&events, &ledger.admitted[0], ended_hold[0])
            && precedes(&events, ended_hold[0], agent[1]),
        "{events:#?}"
    );
}

/// (4) Two firings within one long turn leave exactly one copy delivered: the
/// second supersedes the first, which is withdrawn `superseded` and never
/// delivered.
#[test]
fn two_firings_within_one_long_turn_leave_one_copy_delivered_and_one_superseded() {
    let workspace = Workspace::new();
    let (entered, hold) = (workspace.at("turn-entered"), workspace.at("turn-hold"));
    workspace.write(
        "base.yaml",
        &base_without_done_when(&format!(
            "fake:entered={} fake:hold={}",
            entered.display(),
            hold.display()
        )),
    );
    workspace.graph(&graph(
        Some("{every: 120, start_after: 0}"),
        2,
        &note_entry(NOTE, 2, 0, None),
        "",
    ));
    let gate = workspace.at("note-clock");
    let child = launch(&workspace, &clocked(KEEP_GOING, &gate), None);
    until("the first worker turn to be live", || entered.exists());
    release(&gate);
    until("a second copy to be admitted within the turn", || {
        admitted_yet(&workspace, NOTE, 2)
    });
    release(&hold);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");

    let events = stream(&workspace);
    let ledger = Ledger::of(&events, NOTE);
    let ended = turns_of(&events, "worker", "assistant", "turn-completed")[0];
    let within: Vec<&Value> = ledger
        .admitted
        .iter()
        .filter(|event| precedes(&events, event, ended))
        .collect();
    assert_eq!(
        within.len(),
        2,
        "two firings within the long turn: {events:#?}"
    );
    let (first, second) = (ledger.offer(0), ledger.offer(1));
    let superseded = ledger.withdrawn_for(first);
    assert_eq!(superseded.len(), 1, "{events:#?}");
    assert_eq!(superseded[0]["payload"]["reason"], "superseded");
    assert!(
        ledger.accepted_for(first).is_empty(),
        "the superseded copy was delivered"
    );
    assert_eq!(ledger.accepted_for(second).len(), 1, "{events:#?}");
    assert!(ledger.withdrawn_for(second).is_empty());
    assert!(
        precedes(&events, superseded[0], &ledger.admitted[1]),
        "the earlier copy was not withdrawn before the newer one was admitted"
    );
    assert_eq!(delivered_turns(&events)[0], 2, "{events:#?}");
}

/// (5) A `reset_on` match from the followed stream, with a copy untaken,
/// withdraws that copy `reset`, publishes `cron-reset` naming the envelope that
/// matched, and the next firing waits a whole `every`.
#[test]
fn a_reset_on_match_withdraws_the_untaken_copy_and_the_next_firing_waits_a_full_span() {
    const EVERY: i64 = 3;
    let workspace = Workspace::new();
    let (entered, hold) = (workspace.at("turn-entered"), workspace.at("turn-hold"));
    workspace.write(
        "base.yaml",
        &base_without_done_when(&format!(
            "fake:entered={} fake:hold={}",
            entered.display(),
            hold.display()
        )),
    );
    workspace.graph(&graph(
        Some("{every: 120, start_after: 0}"),
        2,
        &note_entry(NOTE, EVERY as u64, 0, Some(SURFACED)),
        "",
    ));
    let followed = workspace.at("pipeline.jsonl");
    let gate = workspace.at("note-clock");
    let child = launch(&workspace, &clocked(KEEP_GOING, &gate), Some(&followed));
    until("the first worker turn to be live", || entered.exists());
    release(&gate);
    until("the first copy to be admitted", || {
        admitted_yet(&workspace, NOTE, 1)
    });
    append(&followed, &envelope(41, "pipeline", "planner-surfaced"));
    until("the second firing after the reset", || {
        note_fired(&stream(&workspace), NOTE).len() >= 2
    });
    release(&hold);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");

    let events = stream(&workspace);
    let ledger = Ledger::of(&events, NOTE);
    let reset: Vec<&Value> = of(&events, "cron-reset", "worker")
        .into_iter()
        .filter(|event| event["payload"]["scheduled_note"] == NOTE)
        .collect();
    assert_eq!(reset.len(), 1, "{events:#?}");
    assert_eq!(
        reset[0]["payload"]["cause"],
        json!({"source": "pipeline", "kind": "planner-surfaced", "seq": 41,
               "stream": "pipeline-run-1"})
    );
    let first = ledger.offer(0);
    let withdrawn = ledger.withdrawn_for(first);
    assert_eq!(withdrawn.len(), 1, "{events:#?}");
    assert_eq!(withdrawn[0]["payload"]["reason"], "reset");
    assert!(
        ledger.accepted_for(first).is_empty(),
        "the reset copy was delivered"
    );
    assert_eq!(ledger.accepted_for(ledger.offer(1)).len(), 1, "{events:#?}");
    let fired = note_fired(&events, NOTE);
    let waited = gap(reset[0], fired[1]);
    assert!(
        (EVERY * 1000 - 50..EVERY * 1000 + 3000).contains(&waited),
        "the firing after the reset came {waited}ms after it, not a whole every of {EVERY}s"
    );
}

/// (6) A firing after the conversation ended offers nothing: it publishes
/// `cron-fired` saying why, opens no turn, and that clock fires no more.
#[test]
fn a_note_firing_after_the_conversation_ended_says_why_and_its_clock_stops() {
    // A conversation its supervisor passed, and one that reached its turn cap
    // without being passed: each refuses in onejudge's own words.
    for (task, refusal, code, turns) in [
        (
            "fake:complete-now finish at once",
            "conversation-completed",
            0,
            1,
        ),
        ("fake:should-fail never done", "member-settled", 1, 2),
    ] {
        let workspace = Workspace::new();
        let keep_open = workspace.at("keeper-hold");
        let gate = workspace.at("note-clock");
        let skeleton = format!(
            concat!(
                "version: 11\nname: notes\nenv: {{}}\nmembers:\n",
                "  worker:\n    kind: onejudge\n",
                "    base_config: ./base.yaml\n    persona: engineer\n",
                "    agent:\n      oneharness_config: ./oneharness.toml\n",
                "    judge:\n      command: [placeholder]\n",
                "    mode: bypass\n    max_turns: 2\n",
                "    scheduled_notes:\n{notes}",
                "  keeper:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            ),
            notes = note_entry(NOTE, 1, 0, None),
        );
        workspace.graph(&graph_with(
            &skeleton,
            &[
                (FAKE_HARNESS_KEY.to_string(), fake_harness()),
                (format!("members.worker.{FAKE_JUDGE}"), fake_provider()),
                ("members.worker.task".to_string(), clocked(task, &gate)),
                (
                    "members.keeper.task".to_string(),
                    format!("fake:hold={} keep the run open", keep_open.display()),
                ),
            ],
        ));
        let child = launch(&workspace, "unused", None);
        until("the conversation to settle", || {
            !of(&stream(&workspace), "member-settled", "worker").is_empty()
        });
        release(&gate);
        until("the firing after the end", || {
            !note_fired(&stream(&workspace), NOTE).is_empty()
        });
        // Three more of its spans: a clock that went on would fire in each.
        std::thread::sleep(Duration::from_millis(3_000));
        release(&keep_open);
        let (exit, stderr) = exit_of(child);
        assert_eq!(exit, Some(code), "{task}: {stderr}");

        let events = stream(&workspace);
        let fired = note_fired(&events, NOTE);
        assert_eq!(fired.len(), 1, "{task}: the clock fired again: {events:#?}");
        assert_eq!(fired[0]["payload"]["undelivered"], refusal, "{events:#?}");
        let settled = of(&events, "member-settled", "worker")[0];
        assert!(precedes(&events, settled, fired[0]));
        assert!(Ledger::of(&events, NOTE).admitted.is_empty(), "{events:#?}");
        assert_eq!(
            turns_of(&events, "worker", "assistant", "turn-started").len(),
            turns,
            "{task}: the firing opened a turn"
        );
    }
}

/// (7) A firing before a deferred conversation opens is held, admitted when the
/// conversation opens — `note-admitted` before its first `turn-started` — and
/// delivered into that first turn.
#[test]
fn a_note_firing_before_a_deferred_conversation_opens_is_delivered_into_its_first_turn() {
    let workspace = Workspace::new();
    let prompts = workspace.at("agent-prompts");
    // The default base, `done_when` kept, so the judge's completion is what ends
    // the conversation after its one turn; the recorder rides its system prompt.
    workspace.write(
        "base.yaml",
        &BASE.replace(
            "verify before you claim done.\n",
            &format!(
                "verify before you claim done.\n  fake:record-prompt={}\n",
                prompts.display()
            ),
        ),
    );
    // The note as written would not fire for an hour; `--set` addresses it by
    // its id and makes it fire at once, then every second, while the
    // conversation is still waiting to open.
    workspace.graph(&graph(
        Some("{every: 120, start_after: 600}"),
        1,
        &note_entry(NOTE, 1, 3600, None),
        "",
    ));
    let child = launch_with(
        &workspace,
        "fake:complete-now finish at once",
        None,
        &[
            "--set",
            &format!("members.worker.scheduled_notes.{NOTE}.start_after=0"),
        ],
    );
    until(
        "the note to fire twice before the conversation opens",
        || note_fired(&stream(&workspace), NOTE).len() >= 2,
    );
    let early = stream(&workspace);
    assert!(
        Ledger::of(&early, NOTE).admitted.is_empty()
            && turns_of(&early, "worker", "assistant", "turn-started").is_empty(),
        "{early:#?}"
    );
    workspace
        .run(&["trigger", &run_id(&workspace), "worker"])
        .expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");

    let events = stream(&workspace);
    let ledger = Ledger::of(&events, NOTE);
    let first_turn = turns_of(&events, "worker", "assistant", "turn-started")[0];
    // However many firings were held, one copy — the latest — is admitted as
    // the conversation opens; a firing after that is its own copy.
    let before_first: Vec<&Value> = ledger
        .admitted
        .iter()
        .filter(|event| precedes(&events, event, first_turn))
        .collect();
    assert_eq!(before_first.len(), 1, "{events:#?}");
    let opened_with = ledger.offer(0);
    let accepted = ledger.accepted_for(opened_with);
    assert_eq!(accepted.len(), 1, "{events:#?}");
    assert_eq!(accepted[0]["payload"]["accepted"], "queued");
    assert!(ledger.withdrawn_for(opened_with).is_empty());
    let held = note_fired(&events, NOTE);
    assert!(held.len() >= 2 && precedes(&events, held[1], &ledger.admitted[0]));
    assert!(
        precedes(&events, &ledger.admitted[0], first_turn),
        "the held note was not admitted before the first turn: {events:#?}"
    );
    assert_eq!(delivered_turns(&events), vec![1], "{events:#?}");
    let handed = std::fs::read_to_string(&prompts).unwrap_or_default();
    assert!(handed.contains(NOTE_TEXT), "{handed}");
}

/// A clock that declares no `reset_on` keeps a fixed cadence whatever the
/// followed stream and the run itself publish — a member schedule and a
/// scheduled note beside it, across several turn boundaries, with envelopes
/// matching every field of a plausible filter appended throughout.
#[test]
fn clocks_without_reset_on_keep_a_fixed_cadence_whatever_is_followed() {
    const EVERY: i64 = 2;
    const START: i64 = 1;
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    workspace.graph(&graph(
        Some(&format!("{{every: {EVERY}, start_after: 0}}")),
        4,
        &note_entry("tick", EVERY as u64, START as u64, None),
        &single(
            "reporter",
            &format!("{{every: {EVERY}, start_after: {START}}}"),
        ),
    ));
    let followed = workspace.at("pipeline.jsonl");
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let appender = {
        let (followed, done) = (followed.clone(), std::sync::Arc::clone(&done));
        std::thread::spawn(move || {
            let mut seq = 1;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                for kind in ["planner-surfaced", "cron-fired", "turn-completed"] {
                    append(&followed, &envelope(seq, "pipeline", kind));
                    seq += 1;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        })
    };
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        KEEP_GOING,
        "--dir",
        &workspace.dir().display().to_string(),
        "--follow-events",
        &followed.display().to_string(),
    ]);
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    appender.join().expect("the appender ends");
    run.expect_code(1);
    let events = run.events();

    assert!(
        events.iter().all(|event| event["kind"] != "cron-reset"),
        "a clock with no reset_on was reset: {}",
        run.stdout
    );
    assert_eq!(
        turns_of(&events, "worker", "assistant", "turn-started").len(),
        4,
        "{}",
        run.stdout
    );
    let near = |what: &str, waited: i64, span: i64| {
        assert!(
            (span * 1000 - 50..span * 1000 + 2500).contains(&waited),
            "{what} came {waited}ms after its span began, not near {span}s: {}",
            run.stdout
        );
    };
    let started = &events[0];
    assert_eq!(started["kind"], "graph-started");
    let ticks = note_fired(&events, "tick");
    assert!(ticks.len() >= 2, "{}", run.stdout);
    near("the note's first firing", gap(started, ticks[0]), START);
    for pair in ticks.windows(2) {
        near("a later note firing", gap(pair[0], pair[1]), EVERY);
    }
    // A note refired after a turn took its earlier copy is a copy of its own:
    // each is delivered at most once, none is both delivered and withdrawn, and
    // more than one was delivered over the run.
    let ledger = Ledger::of(&events, "tick");
    for index in 0..ledger.admitted.len() {
        let offer = ledger.offer(index);
        let accepted = ledger.accepted_for(offer).len();
        assert!(accepted <= 1, "copy {offer} was delivered {accepted} times");
        assert!(
            accepted == 0 || ledger.withdrawn_for(offer).is_empty(),
            "copy {offer} was delivered and withdrawn"
        );
    }
    assert!(ledger.accepted.len() >= 2, "{}", run.stdout);
    let reporter_up = of(&events, "member-started", "reporter")[0];
    assert_eq!(reporter_up["payload"]["start_after"], json!(START));
    let firings = schedule_fired(&events, "reporter");
    let settles = of(&events, "member-settled", "reporter");
    assert!(firings.len() >= 2, "{}", run.stdout);
    near(
        "the reporter's first firing",
        gap(reporter_up, firings[0]),
        START,
    );
    for (settled, next) in settles.iter().zip(firings.iter().skip(1)) {
        near("a later reporter firing", gap(settled, next), EVERY);
    }
}

/// A matching envelope appended to the followed stream after graph start resets
/// a scheduled note's clock, a paced conversation's hold, and a single-sided
/// schedule that each declare a matching `reset_on`, and `cron-reset` names it.
/// The same envelope written before graph start, an envelope that does not
/// match, and a line that is not an envelope reset nothing — and a `reset_on`
/// matching the run's own events resets with no followed stream at all.
#[test]
fn reset_on_restarts_every_clock_declaring_it_on_a_followed_or_own_envelope() {
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    workspace.graph(&graph(
        Some(&format!(
            "{{every: 120, start_after: 0, reset_on: {SURFACED}}}"
        )),
        2,
        &note_entry("report", 60, 30, Some(SURFACED)),
        &single(
            "reporter",
            &format!("{{every: 60, start_after: 60, reset_on: {SURFACED}}}"),
        ),
    ));
    let followed = workspace.at("pipeline.jsonl");
    append(&followed, &envelope(1, "pipeline", "planner-surfaced"));
    let child = launch(&workspace, KEEP_GOING, Some(&followed));
    until_held(&workspace, 1);
    append(&followed, &envelope(2, "pipeline", "planner-dismissed"));
    append(&followed, &envelope(3, "onevcs", "planner-surfaced"));
    append(&followed, "this line is not an envelope");
    append(&followed, &envelope(4, "pipeline", "planner-surfaced"));
    let resets = |events: &[Value]| -> Vec<Value> {
        events
            .iter()
            .filter(|event| event["kind"] == "cron-reset")
            .cloned()
            .collect()
    };
    until("every clock declaring reset_on to restart", || {
        resets(&stream(&workspace)).len() >= 3
    });
    workspace
        .run(&["trigger", &run_id(&workspace), "worker"])
        .expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("skipped 1 line(s) that were not envelopes"),
        "{stderr}"
    );

    let events = stream(&workspace);
    let resets = resets(&events);
    assert_eq!(resets.len(), 3, "{events:#?}");
    let cause = json!({"source": "pipeline", "kind": "planner-surfaced", "seq": 4,
                       "stream": "pipeline-run-1"});
    let mut restarted: Vec<(String, Value)> = resets
        .iter()
        .map(|event| {
            assert_eq!(event["payload"]["cause"], cause, "{event}");
            (
                event["labels"]["member"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                event["payload"]["scheduled_note"].clone(),
            )
        })
        .collect();
    restarted.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.to_string().cmp(&b.1.to_string())));
    assert_eq!(
        restarted,
        vec![
            ("reporter".to_string(), Value::Null),
            ("worker".to_string(), json!("report")),
            ("worker".to_string(), Value::Null),
        ]
    );
    assert!(
        events.iter().all(|event| event["source"] == "agentgraph"),
        "a followed envelope was re-emitted"
    );

    // The run's own events, with nothing followed.
    let own = Workspace::new();
    own.write("base.yaml", &base_without_done_when(""));
    own.graph(&graph(
        Some("{every: 1, start_after: 0}"),
        2,
        &note_entry("report", 600, 600, None),
        &single(
            "reporter",
            "{every: 60, start_after: 60, reset_on: {include: [{kind: turn-started, member: worker}]}}",
        ),
    ));
    let run = own.run(&[
        "run",
        "./graph.yaml",
        "--task",
        KEEP_GOING,
        "--dir",
        &own.dir().display().to_string(),
        // The run's stream leaves out the very kind the reporter resets on:
        // what a run acts on is unfiltered whatever its stream shows.
        "--event-filter",
        r#"{"exclude": [{"kind": "turn-started"}]}"#,
    ]);
    run.expect_code(1);
    let events = run.events();
    assert!(
        events.iter().all(|event| event["kind"] != "turn-started"),
        "{}",
        run.stdout
    );
    let stream_id = events[0]["stream"].clone();
    let restarted = of(&events, "cron-reset", "reporter");
    assert!(!restarted.is_empty(), "{}", run.stdout);
    for event in restarted {
        let cause = &event["payload"]["cause"];
        assert_eq!(cause["source"], "agentgraph");
        assert_eq!(cause["kind"], "turn-started");
        assert_eq!(cause["stream"], stream_id);
    }
}

/// The deprecated reset still works exactly as before and says it is
/// deprecated: `reset-timer` and `run::signal(.., Signal::Reset)` restart a
/// `resettable` schedule — a single-sided clock at once, and a paced
/// conversation's hold at its next hold — and the same reset leaves a
/// non-resettable schedule and every scheduled-note clock untouched.
#[test]
fn the_deprecated_reset_still_restarts_a_resettable_schedule_and_nothing_else() {
    let workspace = Workspace::new();
    let (entered, hold) = (workspace.at("turn-entered"), workspace.at("turn-hold"));
    workspace.write(
        "base.yaml",
        &base_without_done_when(&format!(
            "fake:entered={} fake:hold={}",
            entered.display(),
            hold.display()
        )),
    );
    let extra = format!(
        "{}{}",
        single("reporter", "{every: 60, start_after: 60, resettable: true}"),
        single("ticker", "{every: 60, start_after: 60}"),
    );
    workspace.graph(&graph(
        Some("{every: 120, start_after: 0, resettable: true}"),
        3,
        &note_entry(NOTE, 3600, 0, None),
        &extra,
    ));

    let validated = workspace.run(&["validate", "./graph.yaml"]);
    validated.expect_code(0);
    let deprecated = |stderr: &str, member: &str| {
        stderr
            .lines()
            .filter(|line| {
                line.contains(&format!("member \"{member}\""))
                    && line.contains("`resettable` is deprecated")
                    && line.contains("`reset_on`")
            })
            .count()
    };
    assert_eq!(
        deprecated(&validated.stderr, "worker"),
        1,
        "{}",
        validated.stderr
    );
    assert_eq!(
        deprecated(&validated.stderr, "reporter"),
        1,
        "{}",
        validated.stderr
    );
    assert_eq!(
        deprecated(&validated.stderr, "ticker"),
        0,
        "{}",
        validated.stderr
    );

    let gate = workspace.at("note-clock");
    let child = launch(&workspace, &clocked(KEEP_GOING, &gate), None);
    until("the first worker turn to be live", || entered.exists());
    release(&gate);
    until("the note to be admitted, untaken", || {
        admitted_yet(&workspace, NOTE, 1)
    });
    let id = run_id(&workspace);
    for member in ["worker", "ticker", "reporter"] {
        let reset = workspace.run(&["reset-timer", &id, member]);
        reset.expect_code(0);
        assert!(
            reset.stderr.contains("`reset-timer` is deprecated")
                && reset.stderr.contains("`reset_on`"),
            "{}",
            reset.stderr
        );
    }
    // A reset is a file, so two inside one tick are one; the library's is left
    // once the verb's has been read.
    until("the verb's reporter reset to land", || {
        !of(&stream(&workspace), "cron-reset", "reporter").is_empty()
    });
    oneagentgraph::run::signal(
        &workspace.state(),
        &RunId::parse(&id).expect("a run id"),
        &MemberName::parse("reporter").expect("a member"),
        Signal::Reset,
    )
    .expect("the library's reset reaches the run");
    // The verb takes what it took before and no more.
    assert_eq!(workspace.run(&["reset-timer", &id, "worker", NOTE]).code, 2);
    assert_eq!(
        workspace
            .run(&["reset-timer", &id, "worker", "--clock", NOTE])
            .code,
        2
    );
    until("both reporter resets to land", || {
        of(&stream(&workspace), "cron-reset", "reporter").len() >= 2
    });
    let mid = stream(&workspace);
    assert!(
        Ledger::of(&mid, NOTE).withdrawn.is_empty(),
        "the reset withdrew a scheduled note's copy: {mid:#?}"
    );
    release(&hold);
    until("the worker's next hold to read its reset", || {
        !of(&stream(&workspace), "cron-reset", "worker").is_empty()
    });
    workspace.run(&["trigger", &id, "worker"]).expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");
    assert_eq!(deprecated(&stderr, "worker"), 1, "{stderr}");
    assert_eq!(deprecated(&stderr, "reporter"), 1, "{stderr}");

    let events = stream(&workspace);
    for (member, count) in [("reporter", 2), ("worker", 1), ("ticker", 0)] {
        let resets = of(&events, "cron-reset", member);
        assert_eq!(resets.len(), count, "{member}: {events:#?}");
        for reset in resets {
            assert_eq!(
                reset["payload"],
                json!({}),
                "the reset changed shape: {reset}"
            );
        }
    }
    let ledger = Ledger::of(&events, NOTE);
    assert!(ledger.withdrawn.is_empty(), "{events:#?}");
    assert_eq!(ledger.admitted.len(), 1);
    assert_eq!(ledger.accepted.len(), 1, "{events:#?}");
}

/// `validate` refuses each new field under schema 10 by its name, a
/// `scheduled_notes` on a single-sided member, a duplicate or ill-formed note
/// id, and a `reset_on` that could match nothing — and accepts a schema-11
/// document carrying all of it.
#[test]
fn validate_gates_scheduled_notes_and_reset_on_and_accepts_them_at_schema_eleven() {
    let workspace = Workspace::new();
    let document = |version: u32, worker: &str, extra: &str| {
        format!(
            concat!(
                "version: {version}\nname: notes\nmembers:\n",
                "  worker:\n    kind: onejudge\n    base_config: ./base.yaml\n",
                "    agent: {{oneharness_config: ./oneharness.toml}}\n",
                "    judge: {{command: [a-judge]}}\n    mode: bypass\n{worker}{extra}",
            ),
            version = version,
            worker = worker,
            extra = extra,
        )
    };
    let notes = format!(
        "    scheduled_notes:\n{}",
        note_entry(NOTE, 60, 5, Some(SURFACED))
    );
    let reset_schedule =
        format!("    schedule: {{every: 60, start_after: 0, reset_on: {SURFACED}}}\n");
    let refused = |text: &str, says: &[&str]| {
        workspace.graph(text);
        let run = workspace.run(&["validate", "./graph.yaml"]);
        run.expect_code(2);
        for word in says {
            assert!(run.stderr.contains(word), "{word}: {}", run.stderr);
        }
    };
    refused(
        &document(10, &notes, ""),
        &["`scheduled_notes`", "requires graph schema version 11"],
    );
    refused(
        &document(10, &reset_schedule, ""),
        &["`schedule.reset_on`", "requires graph schema version 11"],
    );
    refused(
        &document(
            11,
            "",
            &format!(
                "  reporter:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n{notes}"
            ),
        ),
        &["scheduled_notes"],
    );
    let twice = format!(
        "    scheduled_notes:\n{}{}",
        note_entry(NOTE, 60, 5, None),
        note_entry(NOTE, 30, 5, None)
    );
    refused(&document(11, &twice, ""), &["declared twice"]);
    for (entry, says) in [
        (note_entry(NOTE, 0, 5, None), "every 0 seconds"),
        (note_entry(NOTE, 60, 99_999_999_999, None), "`start_after`"),
        (note_entry(NOTE, 99_999_999_999, 5, None), "`every`"),
        (
            note_entry(NOTE, 60, 5, None).replace(NOTE_TEXT, "   "),
            "`text`",
        ),
    ] {
        refused(
            &document(11, &format!("    scheduled_notes:\n{entry}"), ""),
            &[says],
        );
    }
    refused(
        &document(
            11,
            &format!(
                "    scheduled_notes:\n{}",
                note_entry("Nudge_1", 60, 5, None)
            ),
            "",
        ),
        &["\"Nudge_1\""],
    );
    refused(
        &document(
            11,
            &format!(
                "    scheduled_notes:\n{}",
                note_entry(NOTE, 60, 5, Some("{include: [{}]}"))
            ),
            "",
        ),
        &["`reset_on`"],
    );
    refused(
        &document(
            11,
            "    schedule: {every: 60, start_after: 0, reset_on: {}}\n",
            "",
        ),
        &["`schedule.reset_on`", "names no matcher"],
    );

    workspace.graph(&document(11, &format!("{reset_schedule}{notes}"), ""));
    let run = workspace.run(&["validate", "./graph.yaml"]);
    run.expect_code(0);
    assert!(run.stderr.is_empty(), "{}", run.stderr);
}

/// A member schedule's `reset_on` match that lands while a turn is live is held
/// and honoured at the next hold, as a reset is: a scheduled note's clock on the
/// same filter restarts at once, which is how the journey knows the match was
/// read, and the conversation's own `cron-reset` comes only once the hold opens.
#[test]
fn a_reset_on_match_during_a_live_turn_is_honoured_at_the_next_hold() {
    let workspace = Workspace::new();
    let (entered, hold) = (workspace.at("turn-entered"), workspace.at("turn-hold"));
    workspace.write(
        "base.yaml",
        &base_without_done_when(&format!(
            "fake:entered={} fake:hold={}",
            entered.display(),
            hold.display()
        )),
    );
    workspace.graph(&graph(
        Some(&format!(
            "{{every: 120, start_after: 0, reset_on: {SURFACED}}}"
        )),
        2,
        &note_entry("witness", 3600, 3600, Some(SURFACED)),
        "",
    ));
    let followed = workspace.at("pipeline.jsonl");
    let child = launch(&workspace, KEEP_GOING, Some(&followed));
    until("the first worker turn to be live", || entered.exists());
    append(&followed, &envelope(7, "pipeline", "planner-surfaced"));
    let witnessed = |events: &[Value]| {
        of(events, "cron-reset", "worker")
            .into_iter()
            .any(|event| event["payload"]["scheduled_note"] == "witness")
    };
    until("the match to be read", || witnessed(&stream(&workspace)));
    let during = stream(&workspace);
    assert_eq!(
        of(&during, "cron-reset", "worker").len(),
        1,
        "the conversation's hold restarted mid-turn: {during:#?}"
    );
    release(&hold);
    until("the next hold to honour the match", || {
        of(&stream(&workspace), "cron-reset", "worker").len() >= 2
    });
    workspace
        .run(&["trigger", &run_id(&workspace), "worker"])
        .expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");

    let events = stream(&workspace);
    let paced: Vec<&Value> = of(&events, "cron-reset", "worker")
        .into_iter()
        .filter(|event| event["payload"].get("scheduled_note").is_none())
        .collect();
    assert_eq!(paced.len(), 1, "{events:#?}");
    assert_eq!(paced[0]["payload"]["cause"]["seq"], 7);
    let closed = turns_of(&events, "worker", "user", "turn-completed")[0];
    assert!(
        precedes(&events, closed, paced[0]),
        "the hold's reset came before the hold: {events:#?}"
    );
}

/// A `reset_on` match while a firing is held for a conversation that has not
/// opened lets the held copy go: the conversation opens with nothing admitted.
#[test]
fn a_reset_on_match_lets_a_held_copy_go_before_the_conversation_opens() {
    let workspace = Workspace::new();
    workspace.graph(&graph(
        Some("{every: 120, start_after: 600}"),
        1,
        &note_entry(NOTE, 3600, 0, Some(SURFACED)),
        "",
    ));
    let followed = workspace.at("pipeline.jsonl");
    let child = launch(
        &workspace,
        "fake:complete-now finish at once",
        Some(&followed),
    );
    until("the note to fire and be held", || {
        !note_fired(&stream(&workspace), NOTE).is_empty()
    });
    append(&followed, &envelope(9, "pipeline", "planner-surfaced"));
    until("the held note's clock to restart", || {
        !of(&stream(&workspace), "cron-reset", "worker").is_empty()
    });
    workspace
        .run(&["trigger", &run_id(&workspace), "worker"])
        .expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");

    let events = stream(&workspace);
    let ledger = Ledger::of(&events, NOTE);
    assert!(
        ledger.admitted.is_empty(),
        "a copy held before a reset was admitted when the conversation opened: {events:#?}"
    );
    assert!(
        ledger.withdrawn.is_empty(),
        "a copy never admitted was published withdrawn"
    );
    assert_eq!(delivered_turns(&events), Vec::<u64>::new());
    assert_eq!(
        note_fired(&events, NOTE).len(),
        1,
        "the restarted span is a whole hour"
    );
}

/// A member's own `cancel`, and the whole run's `stop`, stop that member's
/// scheduled-note clocks at once — no firing after it, not even one saying the
/// conversation is over — and a member cancel leaves another member keeping the
/// graph alive.
#[test]
fn a_cancel_or_stop_stops_note_clocks_at_once() {
    for scope in [Some("worker"), None] {
        let workspace = Workspace::new();
        let keep_open = workspace.at("keeper-hold");
        let (entered, hold) = (workspace.at("turn-entered"), workspace.at("turn-hold"));
        workspace.write(
            "base.yaml",
            &base_without_done_when(&format!(
                "fake:entered={} fake:hold={}",
                entered.display(),
                hold.display()
            )),
        );
        let keeper = format!(
            "  keeper:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n    task: \
             'fake:hold={} keep the run open'\n",
            keep_open.display()
        );
        workspace.graph(&graph(None, 4, &note_entry(NOTE, 1, 0, None), &keeper));
        let child = launch(&workspace, KEEP_GOING, None);
        until("the worker's turn to be live", || entered.exists());
        until("the note to fire twice", || {
            note_fired(&stream(&workspace), NOTE).len() >= 2
        });
        let id = run_id(&workspace);
        let mut cancel = vec!["cancel", id.as_str()];
        cancel.extend(scope);
        workspace.run(&cancel).expect_code(0);
        until("the worker to end", || {
            !of(&stream(&workspace), "member-died", "worker").is_empty()
        });
        // Three of its spans: a clock that went on would fire in each of them.
        std::thread::sleep(Duration::from_millis(3_000));
        release(&keep_open);
        release(&hold);
        let (code, stderr) = exit_of(child);
        assert_eq!(code, Some(1), "{scope:?}: {stderr}");

        let events = stream(&workspace);
        let died = of(&events, "member-died", "worker")[0];
        let after: Vec<&Value> = note_fired(&events, NOTE)
            .into_iter()
            .filter(|event| precedes(&events, died, event))
            .collect();
        assert!(
            after.is_empty(),
            "{scope:?}: the note clock fired after the stop: {after:#?}"
        );
        if scope.is_some() {
            assert!(
                !of(&events, "member-settled", "keeper").is_empty(),
                "the graph did not live on past the member's cancel"
            );
        }
    }
}

/// A scheduled note may be addressed to the supervisor or to both parties, and
/// each is admitted naming that addressee and delivered exactly once.
#[test]
fn notes_for_the_supervisor_and_for_both_are_admitted_and_delivered_once() {
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    let notes = ["supervisor", "both"]
        .iter()
        .map(|addressee| {
            note_entry(addressee, 3600, 0, None)
                .replace("addressee: worker", &format!("addressee: {addressee}"))
        })
        .collect::<String>();
    workspace.graph(&graph(Some("{every: 120, start_after: 0}"), 2, &notes, ""));
    let gate = workspace.at("note-clock");
    let child = launch(&workspace, &clocked(KEEP_GOING, &gate), None);
    until_held(&workspace, 1);
    release(&gate);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");

    let events = stream(&workspace);
    for addressee in ["supervisor", "both"] {
        let ledger = Ledger::of(&events, addressee);
        assert_eq!(ledger.admitted.len(), 1, "{addressee}: {events:#?}");
        assert_eq!(ledger.admitted[0]["payload"]["addressee"], addressee);
        assert_eq!(ledger.accepted.len(), 1, "{addressee}: {events:#?}");
        assert!(ledger.withdrawn.is_empty(), "{addressee}");
    }
}

/// A detached run forwards `--follow-events` to the run it starts, which resets
/// on what is appended there, and the detaching process says on its own standard
/// error what the graph declares that is deprecated.
#[test]
fn a_detached_run_follows_the_stream_it_was_given_and_warns_of_deprecation() {
    let workspace = Workspace::new();
    let keep_open = workspace.at("keeper-hold");
    let skeleton = concat!(
        "version: 11\nname: detached\nenv: {}\nmembers:\n",
        "  keeper:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
        "  reporter:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
        "    task: write one status update\n",
        "    schedule: {every: 60, start_after: 60, resettable: true,\n",
        "               reset_on: {include: [{source: pipeline, kind: planner-surfaced}]}}\n",
    );
    workspace.graph(&graph_with(
        skeleton,
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            (
                "members.keeper.task".to_string(),
                format!("fake:hold={} keep the run open", keep_open.display()),
            ),
        ],
    ));
    let followed = workspace.at("pipeline.jsonl");
    let detached = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "unused",
        "--dir",
        &workspace.dir().display().to_string(),
        "--follow-events",
        &followed.display().to_string(),
        "--detach",
    ]);
    detached.expect_code(0);
    let warned = detached
        .stderr
        .lines()
        .filter(|line| {
            line.contains("member \"reporter\"")
                && line.contains("`resettable` is deprecated")
                && line.contains("`reset_on`")
        })
        .count();
    assert_eq!(warned, 1, "{}", detached.stderr);
    until("the detached graph to start", || {
        stream(&workspace)
            .iter()
            .any(|event| event["kind"] == "graph-started")
    });
    append(&followed, &envelope(5, "pipeline", "planner-surfaced"));
    until("the reporter's clock to restart", || {
        !of(&stream(&workspace), "cron-reset", "reporter").is_empty()
    });
    release(&keep_open);
    until("the detached run to settle", || {
        stream(&workspace)
            .iter()
            .any(|event| event["kind"] == "graph-settled")
    });
    let events = stream(&workspace);
    let resets = of(&events, "cron-reset", "reporter");
    assert_eq!(resets.len(), 1, "{events:#?}");
    assert_eq!(resets[0]["payload"]["cause"]["seq"], 5);
}

/// The followed stream as another program really writes it: a path that does
/// not exist yet, a line that is not UTF-8, an envelope whose newline has not
/// arrived, and a file replaced by a shorter one. And the run's own `cron-reset`
/// never restarts a clock filtered on `cron-reset`, while one appended to the
/// followed stream — another run's — does.
#[test]
fn a_followed_stream_is_read_as_it_is_written_and_only_anothers_cron_reset_resets() {
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    workspace.graph(&graph(
        Some("{every: 120, start_after: 0}"),
        2,
        &note_entry("echo", 3600, 3600, Some(SURFACED)),
        &single(
            "reporter",
            "{every: 60, start_after: 60, reset_on: {include: [{kind: cron-reset}]}}",
        ),
    ));
    let followed = workspace.at("later.jsonl");
    let child = launch(&workspace, KEEP_GOING, Some(&followed));
    until_held(&workspace, 1);
    let echo_resets = |events: &[Value]| {
        of(events, "cron-reset", "worker")
            .into_iter()
            .filter(|event| event["payload"]["scheduled_note"] == "echo")
            .count()
    };

    // Created after the start; a non-UTF-8 line, then an envelope still being
    // written.
    let mut bytes = b"\xff\xfe not text\n".to_vec();
    bytes.extend_from_slice(envelope(1, "pipeline", "planner-surfaced").as_bytes());
    std::fs::write(&followed, &bytes).expect("the stream is created");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        echo_resets(&stream(&workspace)),
        0,
        "an envelope was read before its newline arrived"
    );
    append(&followed, "");
    until("the completed envelope to reset the note", || {
        echo_resets(&stream(&workspace)) == 1
    });

    // That reset published the run's own `cron-reset`; another run's, followed,
    // is what restarts the reporter.
    let other_run = json!({
        "v": 1, "ts": "2026-11-08T00:00:00.000Z", "stream": "another-run", "seq": 2,
        "source": "agentgraph", "kind": "cron-reset", "labels": {}, "payload": {}
    });
    append(&followed, &other_run.to_string());
    until("the followed cron-reset to restart the reporter", || {
        !of(&stream(&workspace), "cron-reset", "reporter").is_empty()
    });

    // Replaced by a shorter file: read again from its start.
    std::fs::write(
        &followed,
        format!("{}\n", envelope(3, "pipeline", "planner-surfaced")),
    )
    .expect("the stream is replaced");
    until("the replaced stream to be read from its start", || {
        echo_resets(&stream(&workspace)) == 2
    });
    workspace
        .run(&["trigger", &run_id(&workspace), "worker"])
        .expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("skipped 1 line(s) that were not envelopes"),
        "{stderr}"
    );

    let events = stream(&workspace);
    let reporter = of(&events, "cron-reset", "reporter");
    assert_eq!(
        reporter.len(),
        1,
        "the run's own cron-reset restarted a clock: {events:#?}"
    );
    assert_eq!(reporter[0]["payload"]["cause"]["stream"], "another-run");
    let seqs: Vec<&Value> = of(&events, "cron-reset", "worker")
        .into_iter()
        .map(|event| &event["payload"]["cause"]["seq"])
        .collect();
    assert_eq!(seqs, vec![&json!(1), &json!(3)], "{events:#?}");
}

/// Two scheduled notes of one conversation keep clocks of their own: a reset of
/// one withdraws its copy and leaves the other's to be delivered.
#[test]
fn two_notes_of_one_conversation_keep_independent_clocks() {
    let workspace = Workspace::new();
    let (entered, hold) = (workspace.at("turn-entered"), workspace.at("turn-hold"));
    workspace.write(
        "base.yaml",
        &base_without_done_when(&format!(
            "fake:entered={} fake:hold={}",
            entered.display(),
            hold.display()
        )),
    );
    let notes = format!(
        "{}{}",
        note_entry("resets", 3600, 0, Some(SURFACED)),
        note_entry("stands", 3600, 0, None)
    );
    workspace.graph(&graph(Some("{every: 120, start_after: 0}"), 2, &notes, ""));
    let followed = workspace.at("pipeline.jsonl");
    let gate = workspace.at("note-clock");
    let child = launch(&workspace, &clocked(KEEP_GOING, &gate), Some(&followed));
    until("the first worker turn to be live", || entered.exists());
    release(&gate);
    until("both notes to be admitted", || {
        admitted_yet(&workspace, "resets", 1) && admitted_yet(&workspace, "stands", 1)
    });
    append(&followed, &envelope(11, "pipeline", "planner-surfaced"));
    until("the one note's copy to be withdrawn", || {
        !Ledger::of(&stream(&workspace), "resets")
            .withdrawn
            .is_empty()
    });
    release(&hold);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");

    let events = stream(&workspace);
    let (resets, stands) = (Ledger::of(&events, "resets"), Ledger::of(&events, "stands"));
    assert_eq!(resets.withdrawn.len(), 1, "{events:#?}");
    assert_eq!(resets.withdrawn[0]["payload"]["reason"], "reset");
    assert!(resets.accepted.is_empty(), "{events:#?}");
    assert_eq!(stands.admitted.len(), 1, "{events:#?}");
    assert!(
        stands.withdrawn.is_empty(),
        "the other note's copy went with the reset"
    );
    assert_eq!(stands.accepted.len(), 1, "{events:#?}");
    let restarted: Vec<&Value> = of(&events, "cron-reset", "worker");
    assert_eq!(restarted.len(), 1, "{events:#?}");
    assert_eq!(restarted[0]["payload"]["scheduled_note"], "resets");
}

/// A `reset_on` match before a clock's first firing restarts the span it still
/// owes — `start_after`, never `every` — for a scheduled note's clock and for a
/// two-party member's deferred first turn alike.
#[test]
fn a_reset_before_the_first_firing_restores_the_whole_first_span() {
    const START: i64 = 6;
    let near = |what: &str, waited: i64| {
        assert!(
            (START * 1000 - 50..START * 1000 + 3000).contains(&waited),
            "{what} came {waited}ms after the reset, not the whole {START}s it still owed"
        );
    };

    // A scheduled note's clock, `every` one second, owing six.
    let workspace = Workspace::new();
    let (entered, hold) = (workspace.at("turn-entered"), workspace.at("turn-hold"));
    workspace.write(
        "base.yaml",
        &base_without_done_when(&format!(
            "fake:entered={} fake:hold={}",
            entered.display(),
            hold.display()
        )),
    );
    workspace.graph(&graph(
        Some("{every: 120, start_after: 0}"),
        2,
        &note_entry(NOTE, 1, START as u64, Some(SURFACED)),
        "",
    ));
    let followed = workspace.at("pipeline.jsonl");
    let child = launch(&workspace, KEEP_GOING, Some(&followed));
    until("the first worker turn to be live", || entered.exists());
    append(&followed, &envelope(1, "pipeline", "planner-surfaced"));
    until("the first firing after the reset", || {
        !note_fired(&stream(&workspace), NOTE).is_empty()
    });
    release(&hold);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");
    let events = stream(&workspace);
    let reset = of(&events, "cron-reset", "worker");
    assert_eq!(reset.len(), 1, "{events:#?}");
    near(
        "the note's first firing",
        gap(reset[0], note_fired(&events, NOTE)[0]),
    );

    // A deferred conversation's first turn, on its schedule's own `reset_on`.
    let deferred = Workspace::new();
    deferred.graph(&graph(
        Some(&format!(
            "{{every: 1, start_after: {START}, reset_on: {SURFACED}}}"
        )),
        1,
        &note_entry(NOTE, 3600, 3600, None),
        "",
    ));
    let followed = deferred.at("pipeline.jsonl");
    let child = launch(
        &deferred,
        "fake:complete-now finish at once",
        Some(&followed),
    );
    until("the member to come up deferred", || {
        !of(&stream(&deferred), "member-started", "worker").is_empty()
    });
    append(&followed, &envelope(2, "pipeline", "planner-surfaced"));
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");
    let events = stream(&deferred);
    let reset = of(&events, "cron-reset", "worker");
    assert_eq!(reset.len(), 1, "{events:#?}");
    assert_eq!(reset[0]["payload"]["cause"]["seq"], 2);
    let opened = of(&events, "cron-fired", "worker")
        .into_iter()
        .find(|event| event["payload"].get("scheduled_note").is_none())
        .expect("the first turn's delay ended");
    near("the deferred first turn", gap(reset[0], opened));
}
