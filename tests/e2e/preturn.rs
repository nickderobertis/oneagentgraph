//! Pre-turn view journeys against the compiled binary and the real oneharness
//! process.
//!
//! Each of these drives a real member through a real turn with a real pre-turn
//! command: the compiled `oneagentgraph` validates the declared argv, spawns it
//! into the member's own process group, bounds it, drains its pipes, folds what
//! it printed into the prompt, and hands that prompt to real `oneharness`.
//!
//! Every journey asserts first on the **user-facing surface** — the exit code,
//! the `pre-turn-context` and `turn-started` envelopes on stdout, and the
//! `--output text` rendering of them — because that is what a consumer of this
//! CLI actually has.
//!
//! Each then reads what the harness was handed, and that is a second assertion
//! rather than a shortcut past the first. `turn-started` carries the instruction
//! bounded at 4096 bytes, by a contract that predates this field; the prompt is
//! not bounded, and a 16 KiB view is several times that. So the recording is the
//! only place the *whole* instruction is observable, and it is the far end of the
//! path — a context that reached the event but not the harness would be missing
//! there and nowhere else. It is this suite's established way of asking what a
//! member was really given: `tests/e2e/dispatch.rs` proves every task-text
//! journey through the same sentinel.

// llmlint: ignore-file[e2e_not_mocked] these journeys use the repository's sole
// sanctioned fake at oneharness's `ONEHARNESS_BIN_<ID>` paid-provider seam. The
// program a view runs is not a second one: a `pre_turn` command is an argv the
// operator declares, so `oneagentgraph-fake-view` is the *input* under test
// rather than a stand-in for any layer of it — the compiled binary, the real
// spawn, the real bound, and the real oneharness boundary all stay real.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::support::{
    fake_harness, fake_view, graph_with, until, Run, Workspace, FAKE_HARNESS_KEY,
};

/// The bound this build cuts a view's output at, which a journey below drives a
/// view straight past.
const OUTPUT_BOUND: usize = 16 * 1024;

/// A graph with one single-sided member whose `pre_turn` is `views`.
///
/// Every `VIEW` in the skeleton becomes the real program's path, and every
/// `TASK` the member's own prose — placed through the document rather than
/// formatted into it, because a Windows path opens a unicode escape inside a
/// YAML double-quoted scalar and the parser refuses the document. `at` names
/// where each substitution goes, as [`graph_with`] addresses a document.
fn watching_graph(views: &str, at: &[(&str, String)]) -> String {
    let skeleton = format!(
        concat!(
            "version: 7\nname: node-scope\n",
            "env: {{}}\n",
            "members:\n  watcher:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    pre_turn:\n{views}",
        ),
        views = views
    );
    let mut values = vec![(FAKE_HARNESS_KEY.to_string(), fake_harness())];
    values.extend(
        at.iter()
            .map(|(path, value)| ((*path).to_string(), value.clone())),
    );
    graph_with(&skeleton, &values)
}

/// Run the watching graph and answer what it produced.
fn watch(workspace: &Workspace, views: &str, at: &[(&str, String)], task: &str) -> Run {
    workspace.graph(&watching_graph(views, at));
    workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        task,
        "--dir",
        &workspace.dir().display().to_string(),
    ])
}

/// Every prompt the member's harness was given, in the order the turns took
/// them.
///
/// Decoded from the double's own recording shape: one line per recording, with
/// the newlines inside a prompt escaped so a multi-line prompt stays one line.
/// A pre-turn context is several lines by construction, so a journey that read
/// the file raw would be asserting against the encoding rather than the prompt.
fn asked(recorded: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(recorded)
        .unwrap_or_else(|err| panic!("the harness recorded no prompt ({err})"))
        .lines()
        .map(|line| line.replace("\\n", "\n"))
        .collect()
}

/// The one prompt a single-turn member's harness was given.
fn asked_once(recorded: &std::path::Path) -> String {
    let asked = asked(recorded);
    assert_eq!(asked.len(), 1, "the member took {} turns", asked.len());
    asked.into_iter().next().expect("one turn")
}

/// The instruction each of a run's turns opened with, off the run's own stream.
///
/// The user-facing half of every assertion below: `turn-started` carries what
/// the turn was asked, bounded at the contract's 4096 bytes.
fn opened(run: &Run) -> Vec<String> {
    run.of_kind("turn-started")
        .into_iter()
        .filter_map(|event| event["payload"]["instruction"].as_str().map(str::to_string))
        .collect()
}

/// Every `pre-turn-context` payload the run published, in order.
fn views(run: &Run) -> Vec<Value> {
    run.of_kind("pre-turn-context")
        .into_iter()
        .map(|event| event["payload"].clone())
        .collect()
}

/// A view a run could never honour is refused by `validate`, before a member is
/// launched and before a paid turn is spent — with the member, the view, and
/// what is wrong with it named.
///
/// The trust boundary this field sits behind, driven where an operator meets it:
/// a `pre_turn` is an argv this engine hands to a process, and a typo in one is
/// answered by one sentence on standard error rather than by a member that runs
/// a nameless program before every turn, forever.
#[test]
fn a_view_a_run_could_not_honour_is_refused_before_anything_is_launched() {
    let workspace = Workspace::new();
    for (views, expected) in [
        ("    - {command: []}\n", "an empty one names none"),
        ("    - {command: ['   ']}\n", "an empty one names none"),
        (
            "    - {command: [queue-depth], label: '  '}\n",
            "cannot be blank or carry a control character",
        ),
        (
            "    - {command: [queue-depth], timeout: 0}\n",
            "is not a bound this view can run under",
        ),
        (
            "    - {command: [queue-depth], timeout: 3000}\n",
            "is not a bound this view can run under",
        ),
        (
            &"    - {command: [queue-depth]}\n".repeat(5),
            "the ceiling is",
        ),
    ] {
        workspace.graph(&watching_graph(views, &[]));
        let run = workspace.run(&["validate", "./graph.yaml"]);
        run.expect_code(2);
        assert!(
            run.stderr.contains(expected) && run.stderr.contains("watcher"),
            "{views}: {}",
            run.stderr
        );
    }

    // A NUL cannot be written into a YAML document by hand, so it is placed the
    // way any other value is — and refused the same way.
    workspace.graph(&watching_graph(
        "    - {command: [PROGRAM]}\n",
        &[(
            "members.watcher.pre_turn.0.command.0",
            "queue\u{0}depth".to_string(),
        )],
    ));
    let run = workspace.run(&["validate", "./graph.yaml"]);
    run.expect_code(2);
    assert!(run.stderr.contains("carries a NUL"), "{}", run.stderr);

    // And a document declaring the schema before the field is refused by the
    // field's name rather than run with its views silently dropped.
    workspace.graph(
        &watching_graph(
            "    - {command: [PROGRAM]}\n",
            &[(
                "members.watcher.pre_turn.0.command.0",
                "queue-depth".to_string(),
            )],
        )
        .replace("version: 7", "version: 6"),
    );
    let run = workspace.run(&["validate", "./graph.yaml"]);
    run.expect_code(2);
    assert!(
        run.stderr.contains("`pre_turn`") && run.stderr.contains("requires graph schema version 7"),
        "{}",
        run.stderr
    );

    // The same graph with a view a run *can* honour passes, so what these
    // refusals reject is the typo rather than the field.
    workspace.graph(&watching_graph(
        "    - {command: [PROGRAM, --json], label: queue, timeout: 20}\n",
        &[(
            "members.watcher.pre_turn.0.command.0",
            "queue-depth".to_string(),
        )],
    ));
    workspace.run(&["validate", "./graph.yaml"]).expect_code(0);
}

/// A member's declared views run before its turn and their output is what that
/// turn is asked, ahead of the member's own prose.
///
/// The whole capability, end to end and in the order that matters: two views,
/// one named by its author and one named by its program, both run, both folded
/// into the prompt the real harness receives — and the member's own task still
/// there, after them, unchanged. A supervisor's first act becomes reading this
/// rather than spending tool calls rediscovering it.
#[test]
fn a_declared_view_reaches_the_turn_it_was_prepared_for() {
    let workspace = Workspace::new();
    let recorded = workspace.at("watcher.prompt");
    let run = watch(
        &workspace,
        concat!(
            "    - {command: [VIEW, --say, TEXT], label: queue}\n",
            "    - {command: [VIEW, --say, OTHER]}\n",
        ),
        &[
            ("members.watcher.pre_turn.0.command.0", fake_view()),
            (
                "members.watcher.pre_turn.0.command.2",
                "queue depth 4, oldest 12m".to_string(),
            ),
            ("members.watcher.pre_turn.1.command.0", fake_view()),
            (
                "members.watcher.pre_turn.1.command.2",
                "worktree clean".to_string(),
            ),
        ],
        &format!(
            "fake:complete-now report on what the views say. fake:record-prompt={}",
            recorded.display()
        ),
    );
    run.expect_code(0);

    let prompt = asked_once(&recorded);
    assert!(
        prompt.trim_start().starts_with("<pre-turn-context>"),
        "the turn was not opened with its prepared context: {prompt}"
    );
    assert!(
        prompt.contains("<view name=\"queue\">\nqueue depth 4, oldest 12m\n</view>"),
        "the labelled view's output never reached the turn: {prompt}"
    );
    assert!(
        prompt.contains(&format!("<view name=\"{}\">", fake_view())),
        "a view with no label was not named by its program: {prompt}"
    );
    assert!(
        prompt.contains("worktree clean"),
        "the second view never ran: {prompt}"
    );
    let prose = "report on what the views say.";
    assert!(
        prompt
            .find("</pre-turn-context>")
            .zip(prompt.find(prose))
            .is_some_and(|(block, prose)| block < prose),
        "the member's own task did not follow its context: {prompt}"
    );

    // The same two views on the run's own stream, saying what each contributed.
    let published = views(&run);
    assert_eq!(published.len(), 2, "{published:?}");
    assert_eq!(published[0]["label"], "queue");
    assert_eq!(published[0]["outcome"], "captured");
    assert_eq!(published[0]["command"][1], "--say");
    assert!(
        published[0]["bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0),
        "a captured view claimed no bytes: {published:?}"
    );
    assert!(
        published[0].get("truncated").is_none() && published[0].get("detail").is_none(),
        "an uncut view claimed a cut or a reason: {published:?}"
    );

    // And what the operator watching the turn open sees is the context, because
    // `turn-started` carries the instruction the turn really received.
    let opened = opened(&run);
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert!(
        opened[0].contains("<view name=\"queue\">\nqueue depth 4, oldest 12m\n</view>"),
        "the turn opened claiming an instruction it was not given: {}",
        opened[0]
    );
}

/// A view that fails, and one that cannot be started at all, leave the turn
/// happening — and say so where both the model and a reader of the stream can
/// see it.
///
/// The failure this is a fix for is a supervisor reporting on state it never
/// saw, so a view that produced nothing must never be *silently* absent: the
/// turn's own context says the view is unavailable and why, and the run's stream
/// carries the outcome and the reason. The member settles either way — the turn
/// is the valuable thing, and the context is an aid to it.
#[test]
fn a_view_that_fails_leaves_the_turn_happening_and_says_which() {
    let workspace = Workspace::new();
    let recorded = workspace.at("watcher.prompt");
    let run = watch(
        &workspace,
        concat!(
            "    - {command: [VIEW, --complain, TEXT, --fail, '3'], label: queue}\n",
            "    - {command: [MISSING], label: timeline}\n",
            "    - {command: [VIEW], label: silent}\n",
        ),
        &[
            ("members.watcher.pre_turn.0.command.0", fake_view()),
            (
                "members.watcher.pre_turn.0.command.2",
                "no such queue".to_string(),
            ),
            (
                "members.watcher.pre_turn.1.command.0",
                workspace.unreachable_harness(),
            ),
            ("members.watcher.pre_turn.2.command.0", fake_view()),
        ],
        &format!(
            "fake:complete-now report on what the views say. fake:record-prompt={}",
            recorded.display()
        ),
    );
    // The member settled: not one of these is a member failure.
    run.expect_code(0);

    let prompt = asked_once(&recorded);
    for (view, expected) in [
        ("queue", "exited 3: no such queue"),
        ("timeline", "cannot run"),
        ("silent", "printed nothing"),
    ] {
        let element = prompt
            .lines()
            .find(|line| line.contains(&format!("<view name=\"{view}\"")))
            .unwrap_or_else(|| panic!("the {view} view was silently omitted: {prompt}"));
        assert!(
            element.contains("unavailable=\"") && element.contains(expected),
            "the {view} view did not say why it had nothing: {element}"
        );
    }
    assert!(
        prompt.contains("report on what the views say."),
        "the turn lost its own task: {prompt}"
    );

    let published = views(&run);
    let outcomes: Vec<&str> = published
        .iter()
        .filter_map(|view| view["outcome"].as_str())
        .collect();
    assert_eq!(outcomes, vec!["failed", "unspawnable", "empty"]);
    for view in &published {
        assert_eq!(view["bytes"], 0, "{view}");
        assert!(
            view["detail"].as_str().is_some_and(|why| !why.is_empty()),
            "a degraded view published no reason: {view}"
        );
    }
    // The same degradation on the instruction the turn opened with, which is
    // the user-facing envelope carrying it.
    let opened = opened(&run);
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert!(
        opened[0].contains("<view name=\"queue\" unavailable=")
            && opened[0].contains("report on what the views say."),
        "the published instruction did not carry the degraded view: {}",
        opened[0]
    );

    // And rendered for a person on the same terms as a death — the view, what
    // became of it, and the reason — because `--output text` is a rendering of
    // these same events rather than separate content, and a kind with no
    // rendering is one an operator watching a live dispatch never sees.
    let text = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &format!(
            "fake:complete-now report on what the views say. fake:record-prompt={}",
            recorded.display()
        ),
        "--dir",
        &workspace.dir().display().to_string(),
        "--output",
        "text",
    ]);
    text.expect_code(0);
    let rendered = text
        .stdout
        .lines()
        .find(|line| line.contains("pre-turn-context") && line.contains("queue"))
        .unwrap_or_else(|| panic!("the text rendering never carried a view: {}", text.stdout));
    assert!(
        rendered.contains("failed") && rendered.contains("no such queue"),
        "a degraded view rendered without saying what became of it: {rendered}"
    );
}

/// A view that never finishes is stopped at its **own** bound, and the member
/// takes its turn anyway.
///
/// The wedge this exists to prevent: a single-sided member has no per-turn
/// deadline at all, and the activity watchdog does not start until the engine
/// does — so a view with no bound of its own would hold the member forever.
/// Driven under the contract's own production liveness bounds, which is what
/// makes "a member with no per-turn deadline still cannot be wedged" the thing
/// being asserted rather than a shortened watchdog racing the view.
#[test]
fn a_view_that_never_finishes_is_stopped_at_its_own_bound() {
    let workspace = Workspace::new();
    let recorded = workspace.at("watcher.prompt");
    let began = Instant::now();
    let run = watch(
        &workspace,
        "    - {command: [VIEW, --say, TEXT, --hang], label: queue, timeout: 1}\n",
        &[
            ("members.watcher.pre_turn.0.command.0", fake_view()),
            (
                "members.watcher.pre_turn.0.command.2",
                "depth 4".to_string(),
            ),
        ],
        &format!(
            "fake:complete-now report anyway. fake:record-prompt={}",
            recorded.display()
        ),
    );
    run.expect_code(0);
    let spent = began.elapsed();

    let published = views(&run);
    assert_eq!(published.len(), 1, "{published:?}");
    assert_eq!(published[0]["outcome"], "timed_out");
    assert_eq!(published[0]["bytes"], 0);
    assert!(
        published[0]["detail"]
            .as_str()
            .is_some_and(|why| why.contains("did not finish inside 1s")),
        "a stopped view did not name its own bound: {published:?}"
    );

    // The turn happened, and it happened without the view rather than with a
    // half-read one: the output the view managed to print before it wedged is
    // not served as though the view had finished.
    let prompt = asked_once(&recorded);
    assert!(
        prompt.contains("<view name=\"queue\"") && prompt.contains("unavailable=\""),
        "a wedged view was not marked unavailable: {prompt}"
    );
    assert!(
        prompt.contains("report anyway."),
        "the turn lost its own task: {prompt}"
    );
    let opened = opened(&run);
    assert!(
        opened.len() == 1 && opened[0].contains("unavailable=") && opened[0].contains("1s"),
        "the published instruction did not say the view was given up on: {opened:?}"
    );
    assert!(
        spent < Duration::from_secs(120),
        "the member waited {spent:?} on a view bounded at one second"
    );
}

/// A view longer than the bound keeps its opening, is **marked** as cut, and
/// takes its bound's worth of the turn and no more.
///
/// Command output spliced into a model's context is a cost, and a truncation
/// nobody can see is the defect: a view that reads as complete and is not is the
/// same failure as a supervisor reporting on state it only half read.
#[test]
fn a_view_longer_than_the_bound_is_cut_and_says_so_where_the_model_reads_it() {
    let workspace = Workspace::new();
    let recorded = workspace.at("watcher.prompt");
    let run = watch(
        &workspace,
        "    - {command: [VIEW, --bulk, BYTES], label: queue}\n",
        &[
            ("members.watcher.pre_turn.0.command.0", fake_view()),
            (
                "members.watcher.pre_turn.0.command.2",
                (OUTPUT_BOUND * 3).to_string(),
            ),
        ],
        &format!(
            "fake:complete-now report on the view. fake:record-prompt={}",
            recorded.display()
        ),
    );
    run.expect_code(0);

    let published = views(&run);
    assert_eq!(published[0]["outcome"], "captured");
    assert_eq!(published[0]["truncated"], true);
    assert_eq!(
        published[0]["bytes"].as_u64(),
        Some(OUTPUT_BOUND as u64),
        "a cut view carried something other than the bound: {published:?}"
    );

    let prompt = asked_once(&recorded);
    assert!(
        prompt.contains(&format!(
            "<view name=\"queue\" truncated=\"kept the first {OUTPUT_BOUND} bytes\">"
        )),
        "a cut view reached the model reading as a whole one: {}",
        &prompt[..prompt.len().min(400)]
    );
    assert!(
        prompt.contains("row 0 of the prepared view"),
        "the cut kept the wrong end: {}",
        &prompt[..prompt.len().min(400)]
    );
    assert!(
        prompt.contains("report on the view."),
        "the turn lost its own task: {}",
        &prompt[prompt.len().saturating_sub(400)..]
    );
    let opened = opened(&run);
    assert!(
        opened.len() == 1 && opened[0].contains("truncated=\"kept the first"),
        "the published instruction served a cut view as a whole one: {opened:?}"
    );
}

/// A member that declares no view is asked exactly what it always was, and
/// nothing publishes a context it never had.
///
/// The compatibility half, driven rather than argued: every graph document
/// already written declares no view, and this capability must be invisible to
/// all of them.
#[test]
fn a_member_with_no_view_is_asked_exactly_what_it_always_was() {
    let workspace = Workspace::new();
    let recorded = workspace.at("watcher.prompt");
    let task = format!(
        "fake:complete-now write one status update. fake:record-prompt={}",
        recorded.display()
    );
    workspace.graph(&graph_with(
        concat!(
            "version: 7\nname: node-scope\n",
            "env: {}\n",
            "members:\n  watcher:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        &[(FAKE_HARNESS_KEY.to_string(), fake_harness())],
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &task,
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    assert_eq!(
        asked_once(&recorded).trim(),
        task,
        "a member that declared no view had its instruction rewritten"
    );
    assert!(
        run.of_kind("pre-turn-context").is_empty(),
        "a member that declared no view published one: {:?}",
        run.kinds()
    );
    assert_eq!(
        opened(&run),
        vec![task],
        "the published instruction gained something the member never declared"
    );
}

/// A scheduled member runs its views **every turn**, not once for the member.
///
/// This is the shape the capability exists for: a supervisory member paced by a
/// clock, whose whole job is to report on state that has moved since it last
/// looked. A view gathered once and reused would hand every later turn the first
/// turn's state — which is the stale read this is a fix for, reintroduced one
/// layer down.
#[test]
fn a_scheduled_members_views_run_again_for_every_turn() {
    let workspace = Workspace::new();
    let release = workspace.at("release");
    let recorded = workspace.at("watcher.prompt");
    workspace.graph(&graph_with(
        concat!(
            "version: 7\nname: paced\n",
            "env: {}\n",
            "members:\n",
            "  holder:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  watcher:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    schedule: {every: 2, start_after: 1, resettable: true}\n",
            "    pre_turn:\n    - {command: [VIEW, --say, TEXT], label: queue}\n",
        ),
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            (
                "members.watcher.pre_turn.0.command.0".to_string(),
                fake_view(),
            ),
            (
                "members.watcher.pre_turn.0.command.2".to_string(),
                "queue depth 4".to_string(),
            ),
            (
                "members.holder.task".to_string(),
                format!(
                    "fake:complete-now hold this run open. fake:hold={}",
                    release.display()
                ),
            ),
            (
                "members.watcher.task".to_string(),
                format!(
                    "fake:complete-now report on the view. fake:record-prompt={}",
                    recorded.display()
                ),
            ),
        ],
    ));
    // `start_after: 1` rather than `0`, and that is the scheduler's shape rather
    // than this journey's taste: a schedule that takes its turn at t=0 is handed
    // its clock only once the wave it started in has settled, and the wave here
    // holds the run open on purpose. A deferred one gets its clock immediately,
    // which is what lets a second turn come due while the holder is still held.
    let mut child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    let stream = || {
        let mut runs: Vec<_> = std::fs::read_dir(workspace.state())
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("events.jsonl"))
            .collect();
        runs.sort();
        runs.last()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default()
    };
    until("two of the scheduled member's turns", || {
        stream().matches("\"kind\":\"pre-turn-context\"").count() >= 2
    });

    std::fs::write(&release, "release").expect("release the holder");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("waitable").is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("the run finishes");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Both turns opened with the view in front of them, on the run's own stream:
    // a per-member gather would have given that to the first turn only.
    let published = String::from_utf8_lossy(&output.stdout);
    let opened: Vec<String> = published
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        // The watcher's own turns: the member holding the run open takes one too,
        // and it declared no view.
        .filter(|event| event["kind"] == "turn-started" && event["labels"]["member"] == "watcher")
        .filter_map(|event| event["payload"]["instruction"].as_str().map(str::to_string))
        .collect();
    assert!(
        opened.len() >= 2 && opened.iter().all(|it| it.contains("<view name=\"queue\">")),
        "a later turn opened without the view its member declared: {opened:?}"
    );

    // And the same at the far end, where the whole instruction is observable.
    let asked = asked(&recorded);
    assert!(asked.len() >= 2, "the member took {} turns", asked.len());
    for prompt in &asked {
        assert!(
            prompt.contains("<view name=\"queue\">\nqueue depth 4\n</view>"),
            "a turn was asked without the view its member declared: {prompt}"
        );
    }
}
