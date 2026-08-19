//! What a run puts on its merged stream, and what a filter takes off it.
//!
//! Every journey here drives the real binary against a real graph and reads the
//! NDJSON it wrote, because a filter is only meaningful in terms of the stream a
//! consumer actually receives — asserting it against a hand-built envelope would
//! prove the matcher and not the plumbing that consults it.

// llmlint: ignore-file[e2e_not_mocked] see tests/e2e/support.rs: the paid harness
// process is the single sanctioned double, replaced at oneharness's own
// `ONEHARNESS_BIN_<ID>` seam, with real onejudge and real oneharness in between.

use oneagentgraph::config::FIRST_EVENT_FILTER_VERSION;

use crate::support::{
    assert_session_labels, fake_harness, labels, two_party_graph, until, Run, Workspace, NO_ENV,
};

/// The task every journey here runs: one turn, completed, so the stream carries
/// the whole lifecycle a filter then narrows.
const TASK: &str = "fake:complete-now: stream something to filter";

/// The default two-party graph at the schema that has an `events` block, with
/// `body` as that block's `filter`.
///
/// Appended as text rather than assembled through `graph_with`, which carries
/// string scalars only: a filter is lists of mappings, and the point of these
/// journeys is the document an author actually writes.
fn graph_filtering(body: &str) -> String {
    let base = two_party_graph(&fake_harness(), NO_ENV).replace(
        "version: 1",
        &format!("version: {FIRST_EVENT_FILTER_VERSION}"),
    );
    format!("{base}events:\n  filter:\n{body}")
}

/// Run the workspace's graph under `TASK`, with any extra arguments.
fn run_filtered(workspace: &Workspace, extra: &[&str]) -> Run {
    let dir = workspace.dir().display().to_string();
    let mut args = vec!["run", "./graph.yaml", "--task", TASK, "--dir", &dir];
    args.extend_from_slice(extra);
    workspace.run(&args)
}

/// Every kind the stream carried, once each, in the order first seen.
fn distinct(run: &Run) -> Vec<String> {
    let mut seen = Vec::new();
    for kind in run.kinds() {
        if !seen.contains(&kind) {
            seen.push(kind);
        }
    }
    seen
}

/// The whole run's `seq` values, sorted.
///
/// Sorted rather than as written: `seq` is taken before the sink's lock, so two
/// threads publishing at once may reach the wire in the other order. What is
/// being asserted is that the numbers have no gaps in them, which is the promise
/// a consumer's loss detection rests on.
fn seqs(run: &Run) -> Vec<u64> {
    let mut numbers: Vec<u64> = run
        .events()
        .iter()
        .map(|event| event["seq"].as_u64().expect("every envelope is numbered"))
        .collect();
    numbers.sort_unstable();
    numbers
}

/// Assert the stream is numbered `0..n` with nothing missing — what a filtered
/// stream must still be, or every deliberate omission reads as a dropped event.
fn assert_gapless(run: &Run) {
    let numbers = seqs(run);
    assert_eq!(
        numbers,
        (0..numbers.len() as u64).collect::<Vec<_>>(),
        "the stream has a gap in it: {:?}",
        run.kinds()
    );
}

/// A run naming no filter streams exactly what it always did — and so does one
/// naming a filter that admits everything.
///
/// The two are compared against each other rather than against a list written
/// down here, because "unchanged" is a claim about *this* build's stream, not
/// about a lifecycle a future member kind might add to.
#[test]
fn a_run_naming_no_filter_streams_what_it_always_did() {
    let workspace = Workspace::new();
    let unfiltered = run_filtered(&workspace, &[]);
    unfiltered.expect_code(0);
    let admitted = run_filtered(&workspace, &["--event-filter", "{}"]);
    admitted.expect_code(0);

    // Heartbeats are published on a clock rather than on the conversation, so
    // two runs of the same task need not carry the same number of them. Nothing
    // else here is timing-dependent.
    let sequence = |run: &Run| -> Vec<String> {
        run.kinds()
            .into_iter()
            .filter(|kind| kind != "member-heartbeat")
            .collect()
    };
    assert_eq!(sequence(&unfiltered), sequence(&admitted));
    for expected in [
        "graph-started",
        "member-started",
        "turn-started",
        "turn-activity",
        "turn-completed",
        "member-settled",
        "graph-settled",
    ] {
        assert!(
            unfiltered.kinds().iter().any(|kind| kind == expected),
            "an unfiltered run no longer carries {expected}: {:?}",
            unfiltered.kinds()
        );
    }
    assert_gapless(&unfiltered);
    assert_gapless(&admitted);
}

/// An excluded glob takes every kind it spans off the stream and leaves the rest
/// of it — including the numbering — untouched.
#[test]
fn an_excluded_kind_glob_leaves_the_rest_of_the_stream_intact() {
    let workspace = Workspace::new();
    let run = run_filtered(
        &workspace,
        &["--event-filter", r#"{"exclude": [{"kind": "turn-*"}]}"#],
    );
    run.expect_code(0);

    let kinds = distinct(&run);
    assert!(
        !kinds.iter().any(|kind| kind.starts_with("turn-")),
        "the glob left a turn event on the stream: {kinds:?}"
    );
    for kept in [
        "graph-started",
        "member-started",
        "member-settled",
        "graph-settled",
    ] {
        assert!(
            kinds.iter().any(|kind| kind == kept),
            "excluding the turns took {kept} with them: {kinds:?}"
        );
    }
    assert_gapless(&run);
}

/// An include list admits only what it names, and an exclude beside it wins.
#[test]
fn an_include_list_admits_only_what_it_names_and_an_exclude_beats_it() {
    let workspace = Workspace::new();
    let run = run_filtered(
        &workspace,
        &[
            "--event-filter",
            r#"{"include": [{"kind": "turn-*"}, {"kind": "graph-*"}],
                "exclude": [{"kind": "turn-activity"}]}"#,
        ],
    );
    run.expect_code(0);

    let kinds = distinct(&run);
    assert!(
        kinds.iter().any(|kind| kind == "turn-started"),
        "the include list admitted no turn at all: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "graph-started"),
        "{kinds:?}"
    );
    assert!(
        !kinds.iter().any(|kind| kind == "turn-activity"),
        "the exclusion lost to the include list that spans it: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|kind| kind.starts_with("member-")),
        "a kind the include list never named reached the stream: {kinds:?}"
    );
    assert_gapless(&run);
}

/// A matcher on the reserved labels keeps one member's events and drops the
/// graph's own — and a matcher naming a kind no native event carries is accepted
/// as the wire string it is, rather than refused for being unknown here.
#[test]
fn a_filter_matches_member_labels_and_takes_a_kind_this_crate_never_emits() {
    let workspace = Workspace::new();
    let run = run_filtered(
        &workspace,
        &[
            "--event-filter",
            r#"{"include": [{"member": "worker", "persona": "engineer"},
                            {"source": "vcs", "kind": "commit-*"}]}"#,
        ],
    );
    run.expect_code(0);

    let events = run.events();
    assert!(
        !events.is_empty(),
        "the member's own events were all dropped"
    );
    for event in &events {
        let stamped = labels(event);
        assert_eq!(
            (
                stamped.get("member").map(String::as_str),
                stamped.get("persona").map(String::as_str)
            ),
            (Some("worker"), Some("engineer")),
            "an envelope the matcher does not describe reached the stream: {event}"
        );
    }
    let kinds = distinct(&run);
    assert!(
        kinds.iter().any(|kind| kind == "member-started"),
        "{kinds:?}"
    );
    // The graph's own events carry no member, so the matcher does not reach
    // them — and the sibling kind beside it matched nothing this run produced.
    for absent in ["graph-started", "graph-settled", "commit-created"] {
        assert!(
            !kinds.iter().any(|kind| kind == absent),
            "{absent} is not one this filter admits: {kinds:?}"
        );
    }
    assert_gapless(&run);
}

/// The addressing labels a run carries are matchable end to end: the `source`
/// every envelope here is produced under, the `node` and `step` an operator
/// stamped on it, and the `run_id` the run minted for itself.
///
/// `node` and `step` arrive through `--label`, which is how a run this crate
/// composes into a larger pipeline is addressed today — they are reserved keys
/// this crate does not stamp itself, and they reach the wire under exactly the
/// names the grammar names.
#[test]
fn a_filter_matches_the_addressing_labels_a_run_carries() {
    let workspace = Workspace::new();
    let stamped = ["--label", "node=service", "--label", "step=implement"];

    let mut named = vec![
        "--event-filter",
        r#"{"include": [{"source": "agentgraph", "node": "service", "step": "implement"}]}"#,
    ];
    named.extend_from_slice(&stamped);
    let matched = run_filtered(&workspace, &named);
    matched.expect_code(0);
    assert_eq!(
        distinct(&matched),
        distinct(&run_filtered(&workspace, &stamped)),
        "a matcher naming what every envelope carries took something off the stream"
    );
    let run_id = matched.events()[0]["labels"]["run_id"]
        .as_str()
        .expect("every envelope carries the run's id")
        .to_string();

    // One field of the three wrong is the whole matcher wrong, and an include
    // list nothing satisfies leaves an empty stream rather than a full one.
    let mut wrong = vec![
        "--event-filter",
        r#"{"include": [{"node": "service", "step": "review"}]}"#,
    ];
    wrong.extend_from_slice(&stamped);
    let missed = run_filtered(&workspace, &wrong);
    missed.expect_code(0);
    assert!(
        missed.events().is_empty(),
        "a matcher describing no envelope admitted one: {:?}",
        missed.kinds()
    );

    // A run id is minted by the run itself, so what an operator can name is
    // *another* run's: naming it admits nothing here, and excluding it takes
    // nothing away. Either way the matcher was consulted against this run's own
    // stamp rather than ignored.
    let elsewhere = format!(r#"{{"include": [{{"run_id": "{run_id}"}}]}}"#);
    let other_run = run_filtered(&workspace, &["--event-filter", &elsewhere]);
    other_run.expect_code(0);
    assert!(
        other_run.events().is_empty(),
        "a filter naming another run's id admitted this one's events: {:?}",
        other_run.kinds()
    );
    let not_excluded = format!(r#"{{"exclude": [{{"run_id": "{run_id}"}}]}}"#);
    let whole = run_filtered(&workspace, &["--event-filter", &not_excluded]);
    whole.expect_code(0);
    assert!(
        distinct(&whole).iter().any(|kind| kind == "graph-settled"),
        "excluding another run's id emptied this one: {:?}",
        whole.kinds()
    );
    assert_gapless(&whole);
}

/// A graph says what its own run streams, and `--event-filter` overrides it —
/// including for the run a `--detach` leaves behind, which is handed the flag.
#[test]
fn a_graphs_own_filter_holds_until_the_flag_overrides_it() {
    let workspace = Workspace::new();
    workspace.graph(&graph_filtering("    exclude: [{kind: 'graph-*'}]\n"));

    let by_graph = run_filtered(&workspace, &[]);
    by_graph.expect_code(0);
    let kinds = distinct(&by_graph);
    assert!(
        !kinds.iter().any(|kind| kind.starts_with("graph-")),
        "the graph's own filter was not applied: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| kind == "member-started"),
        "{kinds:?}"
    );
    assert_gapless(&by_graph);

    // The flag wins, and it reads a file as readily as an inline document —
    // written the way the same filter is written inside a graph.
    let spec = workspace.write("filter.yaml", "exclude:\n  - {kind: 'member-*'}\n");
    let by_flag = run_filtered(&workspace, &["--event-filter", &spec.display().to_string()]);
    by_flag.expect_code(0);
    let kinds = distinct(&by_flag);
    assert!(
        kinds.iter().any(|kind| kind == "graph-started"),
        "the flag did not displace the graph's own filter: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|kind| kind.starts_with("member-")),
        "the flag's own exclusion was not applied: {kinds:?}"
    );
    assert_gapless(&by_flag);

    // And the detached child is handed the flag: a run left behind under a
    // different stream than the caller asked for is the same failure as one
    // left behind under a different task.
    let detached = run_filtered(
        &workspace,
        &[
            "--event-filter",
            r#"{"exclude": [{"kind": "turn-*"}]}"#,
            "--detach",
        ],
    );
    detached.expect_code(0);
    let started: serde_json::Value =
        serde_json::from_str(detached.stdout.trim()).expect("--detach prints one JSON object");
    let path = started["events_path"].as_str().expect("a path").to_string();
    until("the detached run to settle", || {
        std::fs::read_to_string(&path).is_ok_and(|stream| stream.contains("\"graph-settled\""))
    });
    let stream = std::fs::read_to_string(&path).expect("the detached stream");
    assert!(
        !stream.contains("\"turn-started\""),
        "the detached run did not receive the filter: {stream}"
    );
    assert!(stream.contains("\"member-settled\""), "{stream}");
}

/// A filter a run could not honour is refused before the graph starts, naming
/// the offending matcher — and nothing is launched or recorded.
#[test]
fn an_unusable_filter_is_refused_before_the_graph_starts() {
    let workspace = Workspace::new();
    let unreadable = workspace
        .write("not-a-filter.yaml", "include: 3\n")
        .display()
        .to_string();
    for (spec, expected) in [
        // A matcher naming no field matches every event, so one in `exclude`
        // silences the stream entirely.
        (r#"{"exclude": [{"kind": "turn-*"}, {}]}"#, "exclude[1] {}"),
        // And one naming an empty value matches nothing at all.
        (
            r#"{"include": [{"member": ""}]}"#,
            r#"include[0] {"member":""}"#,
        ),
        (r#"{"include": [{"kind": " "}]}"#, "`kind` is empty"),
        // A spec that is not a filter at all, and a file that is not there.
        (r#"{"includes": []}"#, "includes"),
        ("./nowhere.json", "cannot read --event-filter"),
        // A file that *is* there and is not a filter: read as the document it
        // names, rather than reported as unreadable or silently ignored.
        (unreadable.as_str(), "is not a filter"),
    ] {
        let refused = run_filtered(&workspace, &["--event-filter", spec]);
        refused.expect_code(2);
        assert!(
            refused.stderr.contains(expected),
            "{spec}: the refusal never named the offending matcher: {}",
            refused.stderr
        );
        assert!(
            refused.stdout.trim().is_empty(),
            "{spec}: a refused run still streamed: {}",
            refused.stdout
        );
    }
    assert!(
        std::fs::read_dir(workspace.state())
            .expect("the state directory")
            .next()
            .is_none(),
        "a refused filter still left a run behind"
    );

    // `--detach` refuses it too, rather than reporting a started run whose child
    // then dies out of sight.
    let refused = run_filtered(
        &workspace,
        &["--event-filter", r#"{"exclude": [{}]}"#, "--detach"],
    );
    refused.expect_code(2);
    assert!(
        !refused.stdout.contains("run_id"),
        "a refused --detach printed a detach answer: {}",
        refused.stdout
    );

    // A graph declaring a schema that predates the block is refused by the
    // block's name, rather than run with the stream it did not ask for.
    workspace.graph(
        &graph_filtering("    exclude: [{kind: 'turn-*'}]\n").replace(
            &format!("version: {FIRST_EVENT_FILTER_VERSION}"),
            &format!("version: {}", FIRST_EVENT_FILTER_VERSION - 1),
        ),
    );
    let older = workspace.run(&["validate", "./graph.yaml"]);
    older.expect_code(2);
    assert!(older.stderr.contains("`events`"), "{}", older.stderr);
    assert!(
        older.stderr.contains(&format!(
            "requires graph schema version {FIRST_EVENT_FILTER_VERSION}"
        )),
        "{}",
        older.stderr
    );

    // And a graph at the schema that *has* the block, whose filter parses but
    // could match nothing: refused by the key it is under and the matcher inside
    // it, by `validate` and by the run that would otherwise stream on it.
    workspace.graph(&graph_filtering("    include: [{persona: ''}]\n"));
    for args in [
        vec!["validate", "./graph.yaml"],
        vec!["run", "./graph.yaml", "--task", TASK],
    ] {
        let refused = workspace.run(&args);
        refused.expect_code(2);
        assert!(
            refused.stderr.contains("`events.filter`"),
            "{args:?}: {}",
            refused.stderr
        );
        assert!(
            refused.stderr.contains(r#"include[0] {"persona":""}"#),
            "{args:?}: {}",
            refused.stderr
        );
        assert!(
            refused.stdout.trim().is_empty(),
            "{args:?}: a refused run still streamed: {}",
            refused.stdout
        );
    }
}

/// `--output text` renders the same events, so it renders the filtered ones: a
/// filter that only reached the JSON writer would leave a terminal watching a
/// firehose the caller had already narrowed.
#[test]
fn a_text_rendering_shows_the_filtered_stream_and_nothing_else() {
    let workspace = Workspace::new();
    let run = run_filtered(
        &workspace,
        &[
            "--output",
            "text",
            "--event-filter",
            r#"{"exclude": [{"kind": "turn-*"}]}"#,
        ],
    );
    run.expect_code(0);

    let lines: Vec<&str> = run
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(!lines.is_empty(), "the text rendering was empty");
    // Every line is `<ts> <member> <kind> [detail]`, so the kind is the third
    // word — which is what the filter decided.
    for line in &lines {
        let kind = line.split_whitespace().nth(2).unwrap_or_default();
        assert!(
            !kind.starts_with("turn-"),
            "a rendered line carried an excluded kind: {line}"
        );
    }
    for kept in ["graph-started", "member-settled", "graph-settled"] {
        assert!(
            lines
                .iter()
                .any(|line| line.split_whitespace().nth(2) == Some(kept)),
            "the rendering lost {kept}: {lines:?}"
        );
    }
}

/// A filtered stream still names the conversation of the turns it kept, and a
/// filter that drops the turns takes their conversation with them.
///
/// The two happen at one seam: an envelope's labels are stamped with its
/// conversation and then handed to the filter that decides whether it is written
/// at all. What a consumer receives after narrowing has to still be renderable as
/// a transcript — and the stream has to still be gapless, because a suppressed
/// envelope must not take a `seq` with it, which is the promise the label would
/// otherwise be read against.
#[test]
fn a_filtered_stream_still_names_the_conversation_of_the_turns_it_kept() {
    let workspace = Workspace::new();
    let kept = run_filtered(
        &workspace,
        &["--event-filter", r#"{"include": [{"member": "worker"}]}"#],
    );
    kept.expect_code(0);
    let labelled = assert_session_labels(&kept);
    assert!(
        labelled.contains("turn-started") && labelled.contains("turn-completed"),
        "a narrowed stream lost the conversation of the turns it kept: {labelled:?}"
    );
    assert_gapless(&kept);

    // And the other direction: the turns are the only envelopes carrying one, so
    // excluding them leaves a stream with no conversation on it at all.
    let without = run_filtered(
        &workspace,
        &["--event-filter", r#"{"exclude": [{"kind": "turn-*"}]}"#],
    );
    without.expect_code(0);
    assert!(
        assert_session_labels(&without).is_empty(),
        "an envelope that is not a turn reached the stream with a conversation on it: {:?}",
        without.kinds()
    );
    assert!(
        distinct(&without)
            .iter()
            .any(|kind| kind == "member-settled"),
        "this journey no longer observes the events the exclusion kept: {:?}",
        without.kinds()
    );
    assert_gapless(&without);
}
