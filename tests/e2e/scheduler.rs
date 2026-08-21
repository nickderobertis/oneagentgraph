//! Scheduler journeys against the compiled binary and real oneharness process.

// llmlint: ignore-file[e2e_not_mocked] these journeys use the repository's sole
// sanctioned fake at oneharness's ONEHARNESS_BIN_<ID> paid-provider seam. The
// compiled oneagentgraph and real oneharness CLI/process boundary remain real.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::support::{
    fake_harness, graph_with, until, Workspace, FAKE_HARNESS_KEY, UNREACHABLE_CHAIN,
    UNREACHABLE_HARNESS_KEY,
};

/// The chain every journey below drives, whose `ticker` takes its first turn the
/// moment the graph starts.
///
/// It stays a **version 2** document, where a schedule naming no `start_after`
/// fires at t=0: these journeys are about what a firing *does* — the chain it
/// runs, the failures it propagates, the quiescence that ends it — and each one
/// needs a firing to have happened. What a schedule means from version 4 has its
/// own journeys, starting at
/// [`a_deferred_schedule_starts_with_the_graph_and_takes_no_turn`].
fn scheduled_graph(fake: &str, hold: &str, ticker_config: &str) -> String {
    graph_with(
        concat!(
            "version: 2\nname: scheduled-chain\n",
            "env: {}\n",
            "members:\n",
            "  anchor:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  ticker:\n    kind: oneharness\n",
            "    schedule: {every: 3600}\n",
            "  bridge:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    deps: [anchor]\n",
            "  keeper:\n    kind: onejudge\n    base_config: ./base.yaml\n",
            "    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n    deps: [bridge]\n",
            "  report:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    deps: [ticker]\n",
            "  publish:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    deps: [report]\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake),
            ("members.ticker.oneharness_config", ticker_config),
            (
                "members.keeper.task",
                &format!("fake:complete-now fake:hold={hold}"),
            ),
        ],
    )
}

/// A graph whose scheduled member's first turn is deferred, beside a member that
/// holds the run open while the assertion is made.
///
/// `hold` is released to end the run, and `delays` is the schedule's
/// `(start_after, every)` — with a `None` `start_after` written as a document
/// that names none at all, which is how the default is driven end to end.
///
/// The seconds are substituted into the skeleton rather than passed through
/// [`graph_with`], which writes every value as a string: a schedule's seconds are
/// a number, and one quoted into the document would be refused by the schema
/// before any journey reached what it tests.
fn deferred_graph(fake: &str, hold: &str, delays: (Option<u64>, u64), recorded: &str) -> String {
    const SKELETON: &str = concat!(
        "version: 4\nname: paced\n",
        "env: {}\n",
        "members:\n",
        "  worker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
        "  ticker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
        "    persona: reviewer\n",
        "    schedule: {every: 0, start_after: 0, resettable: true}\n",
    );
    let (start_after, every) = delays;
    let named = match start_after {
        Some(seconds) => format!("start_after: {seconds}, "),
        None => String::new(),
    };
    graph_with(
        &SKELETON
            .replace("start_after: 0, ", &named)
            .replace("every: 0", &format!("every: {every}")),
        &[
            (FAKE_HARNESS_KEY, fake.to_string()),
            (
                "members.worker.task",
                format!("fake:complete-now hold this run open. fake:hold={hold}"),
            ),
            (
                "members.ticker.task",
                format!("fake:complete-now report progress. fake:record-prompt={recorded}"),
            ),
        ],
    )
}

/// A deferred schedule **starts** with the graph and takes **no turn** until its
/// delay elapses — the two halves asserted separately, in one run.
///
/// The distinction the field rests on, and the reason it is not a delayed launch.
/// A graph's scheduled member is the easy one to ship broken — a bad persona ref,
/// an unreadable config, a schedule shape nobody ran — and on a half-hour cadence
/// a member that came up at its first tick would first be heard from half an hour
/// into a real run. So the member comes up here, publishes `member-started` with
/// the argv its first turn will run and the delay that turn is waiting, and has
/// its generated config on disk — while the harness that would spend money is not
/// started at all, which is the second half: no turn, no `cron-fired`, and no
/// prompt recorded by a harness that never ran.
#[test]
fn a_deferred_schedule_starts_with_the_graph_and_takes_no_turn() {
    let workspace = Workspace::new();
    let release = workspace.at("paced-release");
    let recorded = workspace.at("ticker.prompt");
    workspace.graph(&deferred_graph(
        &fake_harness(),
        &release.display().to_string(),
        (Some(3600), 3600),
        &recorded.display().to_string(),
    ));
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    until("both members to start", || {
        ["worker", "ticker"].iter().all(|member| {
            stream(&workspace).lines().any(|line| {
                line.contains("\"kind\":\"member-started\"")
                    && line.contains(&format!("\"member\":\"{member}\""))
            })
        })
    });

    let published = stream(&workspace);
    let started: Value = published
        .lines()
        .filter(|line| {
            line.contains("\"kind\":\"member-started\"") && line.contains("\"member\":\"ticker\"")
        })
        .map(|line| serde_json::from_str(line).expect("an envelope"))
        .next()
        .expect("the deferred member started");
    assert_eq!(started["payload"]["start_after"], 3600);
    assert_eq!(started["payload"]["runner"], "library");
    assert_eq!(started["payload"]["engine"], "oneharness");
    assert_eq!(
        crate::support::labels(&started).get("persona").cloned(),
        Some("reviewer".to_string()),
        "a deferred member that came up must carry the persona it resolved: {started}"
    );
    // Its configuration is not a promise: the generated oneharness config the
    // event names is on disk, which is the work a bad ref would have failed at.
    let config = started["payload"]["config"]
        .as_str()
        .expect("the deferred member names its config");
    assert!(
        std::path::Path::new(config).is_file(),
        "the deferred member's generated config was never written: {config}"
    );

    let no_turn = |stream: &str| {
        assert!(
            !stream.contains("\"kind\":\"cron-fired\""),
            "a deferred schedule fired: {stream}"
        );
        for kind in ["turn-started", "member-settled"] {
            assert!(
                !stream
                    .lines()
                    .any(|line| line.contains(&format!("\"kind\":\"{kind}\""))
                        && line.contains("\"member\":\"ticker\"")),
                "a deferred member published {kind}: {stream}"
            );
        }
        assert!(
            !recorded.exists(),
            "a deferred member's harness ran: {:?}",
            std::fs::read_to_string(&recorded)
        );
    };
    no_turn(&published);

    std::fs::write(&release, "release").expect("release the worker");
    let output = child.wait_with_output().expect("the run finishes");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Still no turn once the run has ended: the delay outlived the run, which is
    // the deferral doing exactly what it says.
    no_turn(&stream(&workspace));
}

/// A deferred schedule's first turn happens once its delay has elapsed, and the
/// turns after it come at `every` rather than at the delay again.
///
/// The other half of the journey above, and what keeps it from being satisfied by
/// a member that simply never fires. Both assertions are floors measured from
/// before the run was launched, so process startup can only push them further out:
/// the first turn cannot fit inside `start_after`, and the second cannot fit
/// inside `start_after + every`. A clock that never left its first-turn delay
/// would take the second turn at twice `start_after`, which is inside that floor
/// and fails here.
#[test]
fn a_deferred_schedule_waits_for_its_delay_and_then_keeps_its_cadence() {
    const START_AFTER: u64 = 1;
    const EVERY: u64 = 4;
    let workspace = Workspace::new();
    let release = workspace.at("paced-release");
    let recorded = workspace.at("ticker.prompt");
    workspace.graph(&deferred_graph(
        &fake_harness(),
        &release.display().to_string(),
        (Some(START_AFTER), EVERY),
        &recorded.display().to_string(),
    ));
    let launched = Instant::now();
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    assert!(
        !recorded.exists(),
        "the member recorded a prompt before the run was even launched"
    );
    // One line per turn: the double writes the whole prompt it was given, with
    // its newlines escaped, so a turn is a line.
    let turns = || {
        std::fs::read_to_string(&recorded)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains("report progress"))
            .count()
    };
    until("the deferred first turn", || turns() >= 1);
    let first = launched.elapsed();
    assert!(
        first >= Duration::from_secs(START_AFTER),
        "the first turn came {first:?} after launch, sooner than the delay it named"
    );

    until("the turn after it", || turns() >= 2);
    let second = launched.elapsed();
    assert!(
        second >= Duration::from_secs(START_AFTER + EVERY),
        "the second turn came {second:?} after launch, sooner than the cadence allows"
    );

    std::fs::write(&release, "release").expect("release the worker");
    let output = child.wait_with_output().expect("the run finishes");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A schedule that names no `start_after` waits one whole `every` before its
/// first turn — the default, driven end to end.
///
/// The behaviour every schedule already written now has, so it is asserted
/// through a document that names the field nowhere rather than through one that
/// asks for the default by writing it. What the run publishes is the delay it
/// derived, and the turn lands no sooner than that.
#[test]
fn a_schedule_that_names_no_delay_waits_one_whole_interval() {
    const EVERY: u64 = 3;
    let workspace = Workspace::new();
    let release = workspace.at("paced-release");
    let recorded = workspace.at("ticker.prompt");
    let document = deferred_graph(
        &fake_harness(),
        &release.display().to_string(),
        (None, EVERY),
        &recorded.display().to_string(),
    );
    assert!(
        !document.contains("start_after"),
        "this journey is about the document that names no delay: {document}"
    );
    workspace.graph(&document);
    let launched = Instant::now();
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    until("the deferred first turn", || recorded.is_file());
    let waited = launched.elapsed();
    assert!(
        waited >= Duration::from_secs(EVERY),
        "the first turn came {waited:?} after launch, so a schedule naming no delay took one \
         at t=0"
    );
    let started: Value = stream(&workspace)
        .lines()
        .find(|line| {
            line.contains("\"kind\":\"member-started\"") && line.contains("\"member\":\"ticker\"")
        })
        .map(|line| serde_json::from_str(line).expect("an envelope"))
        .expect("the deferred member started");
    assert_eq!(
        started["payload"]["start_after"], EVERY,
        "the delay a schedule inherits from `every` must be the one it publishes"
    );

    std::fs::write(&release, "release").expect("release the worker");
    let output = child.wait_with_output().expect("the run finishes");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A deferred member that waits on a dependency comes up — and starts counting —
/// when its own wave is reached, not before.
///
/// The honest limit of "a deferred member starts with the graph": a member with
/// `deps` starts when its dependencies have settled, which is the earliest it
/// could have run anything either way. Asserted as order in the stream rather
/// than as a duration, because what is claimed is a boundary rather than a delay:
/// the member is announced *after* its dependency settled, and takes its turn
/// after that.
#[test]
fn a_deferred_member_with_dependencies_comes_up_when_its_wave_is_reached() {
    let workspace = Workspace::new();
    let release = workspace.at("keeper-release");
    let recorded = workspace.at("ticker.prompt");
    workspace.graph(&graph_with(
        concat!(
            "version: 4\nname: paced-chain\n",
            "env: {}\n",
            "members:\n",
            "  anchor:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  keeper:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    deps: [anchor]\n",
            "  ticker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    deps: [anchor]\n",
            "    schedule: {every: 3600, start_after: 1}\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            (
                "members.anchor.task",
                "fake:complete-now anchor this run.".to_string(),
            ),
            (
                "members.keeper.task",
                format!(
                    "fake:complete-now hold this run open. fake:hold={}",
                    release.display()
                ),
            ),
            (
                "members.ticker.task",
                format!(
                    "fake:complete-now report progress. fake:record-prompt={}",
                    recorded.display()
                ),
            ),
        ],
    ));
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    until("the deferred member's own turn", || recorded.is_file());

    // The order the stream carries: the dependency settled, then this member came
    // up, then it took the turn its delay was counting down to.
    let published = stream(&workspace);
    let at = |kind: &str, member: &str| -> usize {
        published
            .lines()
            .position(|line| {
                line.contains(&format!("\"kind\":\"{kind}\""))
                    && line.contains(&format!("\"member\":\"{member}\""))
            })
            .unwrap_or_else(|| panic!("no {kind} for {member} in\n{published}"))
    };
    assert!(
        at("member-settled", "anchor") < at("member-started", "ticker"),
        "a member waiting on a dependency was announced before that dependency \
         settled:\n{published}"
    );
    assert!(
        at("member-started", "ticker") < at("cron-fired", "ticker"),
        "the deferred turn was taken before the member came up:\n{published}"
    );

    std::fs::write(&release, "release").expect("release the keeper");
    let output = child.wait_with_output().expect("the run finishes");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `cancel` stops a deferred member before its first turn, and the turn never
/// happens.
///
/// A clock that starts before its member's first turn is a clock an operator has
/// to be able to stop, and the delay is exactly when they would want to: a
/// pacemaker shipped with the wrong persona should be stoppable without waiting
/// out its cadence to watch it run. The delay here is short enough that the turn
/// would have happened well inside this journey, so "no turn" is a cancel that
/// landed rather than a wait that outlived the run.
#[test]
fn a_cancel_stops_a_deferred_member_before_its_first_turn() {
    const START_AFTER: u64 = 2;
    let workspace = Workspace::new();
    let release = workspace.at("paced-release");
    let recorded = workspace.at("ticker.prompt");
    workspace.graph(&deferred_graph(
        &fake_harness(),
        &release.display().to_string(),
        (Some(START_AFTER), 3600),
        &recorded.display().to_string(),
    ));
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    until("the deferred member to come up", || {
        stream(&workspace).lines().any(|line| {
            line.contains("\"kind\":\"member-started\"") && line.contains("\"member\":\"ticker\"")
        })
    });
    let id = workspace.record()["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    workspace.run(&["cancel", &id, "ticker"]).expect_code(0);

    // Well past the delay the cancelled clock was counting down.
    std::thread::sleep(Duration::from_secs(START_AFTER + 3));
    assert!(
        !recorded.exists(),
        "a cancelled member took its deferred turn anyway: {:?}",
        std::fs::read_to_string(&recorded)
    );

    std::fs::write(&release, "release").expect("release the worker");
    let output = child.wait_with_output().expect("the run finishes");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A deferred first turn that fails is the run's failure, reported and counted.
///
/// The route a failure takes is not the one a schedule firing at t=0 takes. That
/// one settles inside its wave, where `run` records it directly; a deferred turn
/// happens on the member's own clock thread and reaches the record and the exit
/// status through the channel that thread reports on. A member that died quietly
/// there would be a graph that exits 0 with a dead member in it, which is the
/// failure shape this whole node exists to close.
#[test]
fn a_deferred_first_turn_that_fails_fails_the_run() {
    let workspace = Workspace::new();
    let release = workspace.at("paced-release");
    let recorded = workspace.at("ticker.prompt");
    // A chain whose one identity this journey gives an unreachable binary for:
    // oneharness reaches nothing and the member dies, which is a death rather
    // than a task it drove and did not finish.
    workspace.write("failing.toml", UNREACHABLE_CHAIN);
    workspace.graph(&graph_with(
        &deferred_graph(
            &fake_harness(),
            &release.display().to_string(),
            (Some(1), 3600),
            &recorded.display().to_string(),
        )
        .replace(
            "  ticker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml",
            "  ticker:\n    kind: oneharness\n    oneharness_config: ./failing.toml",
        ),
        &[(UNREACHABLE_HARNESS_KEY, workspace.unreachable_harness())],
    ));
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    until("the deferred turn to die", || {
        stream(&workspace).lines().any(|line| {
            line.contains("\"kind\":\"member-died\"") && line.contains("\"member\":\"ticker\"")
        })
    });
    assert!(
        !recorded.exists(),
        "the failing member ran a turn after all: {:?}",
        std::fs::read_to_string(&recorded)
    );

    std::fs::write(&release, "release").expect("release the worker");
    let output = child.wait_with_output().expect("the run finishes");
    assert_eq!(
        output.status.code(),
        Some(1),
        "a dead member on a deferred clock did not fail the run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        workspace.record()["members"]["ticker"]
            .as_str()
            .unwrap_or_default(),
        "died (provider-failure)",
        "the record kept no outcome for the member its clock reported: {}",
        workspace.record()
    );
}

/// Under version 4, `start_after: 0` takes the first turn in the wave and then
/// keeps the cadence.
///
/// The opt-out, driven where the default is the other answer. A schedule asking
/// for t=0 must reach its turn through the ordinary wave rather than through a
/// clock that deferred it by zero seconds — which is visible in the stream: a
/// member that came up without taking a turn names the delay it is waiting, and
/// this one has no delay to name.
///
/// The member holding the run open waits on `anchor` so it lands in the *second*
/// wave. A schedule that fires at t=0 hands its clock over only once its own wave
/// has settled, so a held member beside it in the first wave would keep that
/// clock from ever starting — which is the shape the journeys in
/// `tests/e2e/verbs.rs` use for the same reason.
#[test]
fn an_explicit_zero_delay_takes_its_first_turn_in_the_wave() {
    const EVERY: u64 = 2;
    let workspace = Workspace::new();
    let release = workspace.at("keeper-release");
    let recorded = workspace.at("ticker.prompt");
    workspace.graph(&graph_with(
        concat!(
            "version: 4\nname: paced-at-once\n",
            "env: {}\n",
            "members:\n",
            "  anchor:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  ticker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    schedule: {every: 2, start_after: 0}\n",
            "  keeper:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    deps: [anchor]\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            (
                "members.anchor.task",
                "fake:complete-now anchor this run.".to_string(),
            ),
            (
                "members.keeper.task",
                format!(
                    "fake:complete-now hold this run open. fake:hold={}",
                    release.display()
                ),
            ),
            (
                "members.ticker.task",
                format!(
                    "fake:complete-now report progress. fake:record-prompt={}",
                    recorded.display()
                ),
            ),
        ],
    ));
    let launched = Instant::now();
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    let turns = || {
        std::fs::read_to_string(&recorded)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains("report progress"))
            .count()
    };
    until("the first turn", || turns() >= 1);

    let started: Value = stream(&workspace)
        .lines()
        .find(|line| {
            line.contains("\"kind\":\"member-started\"") && line.contains("\"member\":\"ticker\"")
        })
        .map(|line| serde_json::from_str(line).expect("an envelope"))
        .expect("the member started");
    assert!(
        started["payload"].get("start_after").is_none(),
        "a member that took its turn in the wave named a delay it never waited: {started}"
    );

    // And the clock it hands over keeps the cadence, so `start_after: 0` is a
    // first turn rather than a schedule that fires once.
    until("the turn after it", || turns() >= 2);
    let second = launched.elapsed();
    assert!(
        second >= Duration::from_secs(EVERY),
        "the second turn came {second:?} after launch, sooner than the cadence allows"
    );

    std::fs::write(&release, "release").expect("release the keeper");
    let output = child.wait_with_output().expect("the run finishes");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `trigger` fires a member whose first turn is still deferred, now.
///
/// An operator is not locked out for the length of the delay. The pair with the
/// journey above is the point: the same graph that takes no turn for an hour takes
/// one within seconds when somebody asks for it, so a half-hour pacemaker can
/// still be made to report on demand.
#[test]
fn a_trigger_fires_a_member_whose_first_turn_is_still_deferred() {
    let workspace = Workspace::new();
    let release = workspace.at("paced-release");
    let recorded = workspace.at("ticker.prompt");
    workspace.graph(&deferred_graph(
        &fake_harness(),
        &release.display().to_string(),
        (Some(3600), 3600),
        &recorded.display().to_string(),
    ));
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    until("the deferred member to come up", || {
        stream(&workspace).lines().any(|line| {
            line.contains("\"kind\":\"member-started\"") && line.contains("\"member\":\"ticker\"")
        })
    });
    let id = workspace.record()["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    workspace.run(&["trigger", &id, "ticker"]).expect_code(0);
    until("the triggered turn", || recorded.is_file());
    assert!(
        stream(&workspace).contains("\"kind\":\"cron-fired\""),
        "a triggered turn was taken without saying so: {}",
        stream(&workspace)
    );

    std::fs::write(&release, "release").expect("release the worker");
    let output = child.wait_with_output().expect("the run finishes");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `reset-timer` before the first turn restarts the whole of `start_after`,
/// rather than promoting the member to its steady cadence.
///
/// What a reset means is "restart the wait in progress", and before the first turn
/// the wait in progress is the first-turn delay. The floor is measured from the
/// moment the reset was *asked for* — the run picks it up strictly after that — so
/// a member that ignored the reset would fire at launch plus the delay, which is
/// earlier than this floor and fails here.
#[test]
fn a_reset_before_the_first_turn_restarts_the_whole_delay() {
    const START_AFTER: u64 = 4;
    let workspace = Workspace::new();
    let release = workspace.at("paced-release");
    let recorded = workspace.at("ticker.prompt");
    workspace.graph(&deferred_graph(
        &fake_harness(),
        &release.display().to_string(),
        (Some(START_AFTER), 3600),
        &recorded.display().to_string(),
    ));
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    until("the deferred member to come up", || {
        stream(&workspace).lines().any(|line| {
            line.contains("\"kind\":\"member-started\"") && line.contains("\"member\":\"ticker\"")
        })
    });
    // Well into the delay, so that "the reset restarted it" and "the reset did
    // nothing" are seconds apart rather than milliseconds.
    std::thread::sleep(Duration::from_secs(2));
    let id = workspace.record()["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    assert!(
        !recorded.exists(),
        "the member took its turn before the reset this journey is about"
    );
    let asked = Instant::now();
    workspace
        .run(&["reset-timer", &id, "ticker"])
        .expect_code(0);
    until("the run to pick the reset up", || {
        stream(&workspace).contains("\"kind\":\"cron-reset\"")
    });
    until("the restarted first turn", || recorded.is_file());
    let waited = asked.elapsed();
    assert!(
        waited >= Duration::from_secs(START_AFTER),
        "the first turn came {waited:?} after the reset, so the reset did not restart the delay"
    );

    std::fs::write(&release, "release").expect("release the worker");
    let output = child.wait_with_output().expect("the run finishes");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// This run's merged event stream, as it stands.
fn stream(workspace: &Workspace) -> String {
    std::fs::read_dir(workspace.state())
        .into_iter()
        .flatten()
        .flatten()
        .next()
        .map(|entry| entry.path().join("events.jsonl"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default()
}

#[test]
fn cron_firings_repeat_the_chain_and_quiescence_finishes_it() {
    let workspace = Workspace::new();
    let release = workspace.at("release");
    workspace.graph(&scheduled_graph(
        &fake_harness(),
        &release.display().to_string(),
        "./oneharness.toml",
    ));
    let mut child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: scheduled",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    let events = || {
        let mut paths: Vec<_> = std::fs::read_dir(workspace.state())
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("events.jsonl"))
            .collect();
        paths.sort();
        paths
            .last()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default()
    };
    let start_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let stream = events();
        if stream.contains("\"member\":\"keeper\"")
            && stream.matches("\"member\":\"report\"").count() >= 2
        {
            break;
        }
        if child.try_wait().expect("waitable").is_some() || Instant::now() >= start_deadline {
            let output = child.wait_with_output().expect("failed output");
            panic!(
                "run exited before its later wave: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    for expected in 1..=2 {
        let run = workspace.record();
        let id = run["run_id"].as_str().expect("run id");
        workspace.run(&["trigger", id, "ticker"]).expect_code(0);
        until("the cron firing and its chain", || {
            let stream = events();
            stream.matches("\"kind\":\"cron-fired\"").count() >= expected
                && stream.matches("\"member\":\"report\"").count() >= (expected + 2) * 2
        });
    }
    std::fs::write(&release, "release").expect("release keeper");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("waitable").is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        child.try_wait().expect("waitable").is_some(),
        "run did not quiesce"
    );
    let output = child.wait_with_output().expect("finished output");
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn a_failed_downstream_member_suppresses_its_dependant_in_that_cron_iteration() {
    let workspace = Workspace::new();
    let release = workspace.at("failure-release");
    let marker = workspace.at("report-first-run");
    workspace.write(
        "report.toml",
        "run_mode = \"fallback\"\nharnesses = [\"codex\"]\n",
    );
    let graph = scheduled_graph(
        &fake_harness(),
        &release.display().to_string(),
        "./oneharness.toml",
    )
    .replace(
        "report:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml",
        "report:\n    kind: oneharness\n    oneharness_config: ./report.toml",
    );
    let graph = graph_with(
        &graph,
        &[
            ("env.ONEHARNESS_BIN_CODEX", fake_harness()),
            (
                "env.FAKE_HARNESS_FAIL_AFTER_MARKER",
                marker.display().to_string(),
            ),
        ],
    );
    workspace.graph(&graph);
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: scheduled failure",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    let events_path = || {
        std::fs::read_dir(workspace.state())
            .into_iter()
            .flatten()
            .flatten()
            .next()
            .map(|entry| entry.path().join("events.jsonl"))
    };
    until("the initial chain to settle", || {
        events_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|stream| {
                stream.contains("\"member\":\"keeper\"")
                    && stream.lines().any(|line| {
                        line.contains("\"kind\":\"member-settled\"")
                            && line.contains("\"member\":\"publish\"")
                    })
            })
    });
    let id = workspace.record()["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    workspace.run(&["trigger", &id, "ticker"]).expect_code(0);
    until("the later report to fail", || {
        events_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|stream| {
                stream.contains("\"kind\":\"cron-fired\"")
                    && stream.lines().any(|line| {
                        line.contains("\"kind\":\"member-died\"")
                            && line.contains("\"member\":\"report\"")
                    })
            })
    });
    let stream = std::fs::read_to_string(events_path().expect("events")).expect("stream");
    assert_eq!(
        stream
            .lines()
            .filter(|line| {
                line.contains("\"kind\":\"member-started\"")
                    && line.contains("\"member\":\"report\"")
            })
            .count(),
        2,
        "the later firing never reached the failing downstream member: {stream}"
    );
    assert_eq!(
        stream
            .lines()
            .filter(|line| {
                line.contains("\"kind\":\"member-started\"")
                    && line.contains("\"member\":\"publish\"")
            })
            .count(),
        1,
        "the failed downstream member started its dependant: {stream}"
    );
    std::fs::write(&release, "release").expect("release keeper");
    let output = child.wait_with_output().expect("run finishes");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn a_failed_later_cron_firing_suppresses_that_iterations_chain() {
    let workspace = Workspace::new();
    let release = workspace.at("root-failure-release");
    let marker = workspace.at("ticker-first-run");
    workspace.write(
        "ticker.toml",
        "run_mode = \"fallback\"\nharnesses = [\"codex\"]\n",
    );
    let graph = scheduled_graph(
        &fake_harness(),
        &release.display().to_string(),
        "./ticker.toml",
    );
    let graph = graph_with(
        &graph,
        &[
            ("env.ONEHARNESS_BIN_CODEX", fake_harness()),
            (
                "env.FAKE_HARNESS_FAIL_AFTER_MARKER",
                marker.display().to_string(),
            ),
        ],
    );
    workspace.graph(&graph);
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: scheduled root failure",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    let events_path = || {
        std::fs::read_dir(workspace.state())
            .into_iter()
            .flatten()
            .flatten()
            .next()
            .map(|entry| entry.path().join("events.jsonl"))
    };
    until("the initial chain to settle", || {
        events_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|stream| {
                stream.lines().any(|line| {
                    line.contains("\"kind\":\"member-settled\"")
                        && line.contains("\"member\":\"publish\"")
                })
            })
    });
    let id = workspace.record()["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    workspace.run(&["trigger", &id, "ticker"]).expect_code(0);
    until("the later cron firing to fail", || {
        events_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|stream| {
                stream.contains("\"kind\":\"cron-fired\"")
                    && stream.lines().any(|line| {
                        line.contains("\"kind\":\"member-died\"")
                            && line.contains("\"member\":\"ticker\"")
                    })
            })
    });
    let stream = std::fs::read_to_string(events_path().expect("events")).expect("stream");
    assert_eq!(
        stream
            .lines()
            .filter(|line| {
                line.contains("\"kind\":\"member-started\"")
                    && line.contains("\"member\":\"report\"")
            })
            .count(),
        1,
        "the failed cron firing started its chain: {stream}"
    );
    std::fs::write(&release, "release").expect("release keeper");
    let output = child.wait_with_output().expect("run finishes");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn a_cron_only_graph_quiesces_after_its_initial_firing() {
    let workspace = Workspace::new();
    workspace.graph(&graph_with(
        concat!(
            "version: 1\nname: cron-only\n",
            "env: {}\n",
            "members:\n  ticker:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    schedule: {every: 3600}\n",
            "  report:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n    deps: [ticker]\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
    ));
    let started = Instant::now();
    let run = workspace.run_task("fake:complete-now: cron only");
    run.expect_code(0);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(run.of_kind("cron-fired").is_empty());
    assert_eq!(
        run.of_kind("member-started")
            .iter()
            .filter(|event| {
                crate::support::labels(event)
                    .get("member")
                    .map(String::as_str)
                    == Some("report")
            })
            .count(),
        1
    );
}

#[test]
fn a_failed_initial_scheduled_run_skips_its_chain_and_settles() {
    let workspace = Workspace::new();
    workspace.write("failing.toml", UNREACHABLE_CHAIN);
    workspace.graph(&graph_with(
        "version: 1\nname: failed-initial-cron\nenv: {}\nmembers:\n  ticker:\n    kind: oneharness\n    oneharness_config: ./failing.toml\n    schedule: {every: 3600}\n  report:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n    deps: [ticker]\n",
        &[(UNREACHABLE_HARNESS_KEY, workspace.unreachable_harness())],
    ));
    let run = workspace.run_task("fake:complete-now: failed initial schedule");
    run.expect_code(1);
    assert!(run.of_kind("cron-fired").is_empty());
    assert!(run.of_kind("member-started").iter().all(|event| {
        crate::support::labels(event)
            .get("member")
            .map(String::as_str)
            != Some("report")
    }));
    assert_eq!(
        run.of_kind("graph-settled")[0]["payload"]["members"]["report"],
        "skipped (ticker)"
    );
}

#[test]
fn a_failed_initial_scheduled_run_can_fire_again_while_non_cron_work_is_live() {
    let workspace = Workspace::new();
    let release = workspace.at("initial-failure-release");
    let failed_once = workspace.at("ticker-failed-once");
    workspace.write(
        "ticker.toml",
        "run_mode = \"fallback\"\nharnesses = [\"codex\"]\n",
    );
    let graph = scheduled_graph(
        &fake_harness(),
        &release.display().to_string(),
        "./ticker.toml",
    );
    let graph = graph_with(
        &graph,
        &[
            ("env.ONEHARNESS_BIN_CODEX", fake_harness()),
            (
                "env.FAKE_HARNESS_FAIL_ONCE_MARKER",
                failed_once.display().to_string(),
            ),
        ],
    );
    workspace.graph(&graph);
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: initial scheduled failure recovers",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    let events_path = || {
        std::fs::read_dir(workspace.state())
            .into_iter()
            .flatten()
            .flatten()
            .next()
            .map(|entry| entry.path().join("events.jsonl"))
    };
    until("the initial ticker failure and live keeper", || {
        events_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|stream| {
                stream.contains("\"member\":\"keeper\"")
                    && stream.lines().any(|line| {
                        line.contains("\"kind\":\"member-died\"")
                            && line.contains("\"member\":\"ticker\"")
                    })
            })
    });
    let id = workspace.record()["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    workspace.run(&["trigger", &id, "ticker"]).expect_code(0);
    until("the recovered firing to run its chain", || {
        events_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|stream| {
                stream.contains("\"kind\":\"cron-fired\"")
                    && stream.lines().any(|line| {
                        line.contains("\"kind\":\"member-started\"")
                            && line.contains("\"member\":\"report\"")
                    })
            })
    });
    std::fs::write(&release, "release").expect("release keeper");
    let output = child.wait_with_output().expect("run finishes");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn cron_iterations_keep_failed_independent_dependencies_blocked() {
    let workspace = Workspace::new();
    let release = workspace.at("independent-failure-release");
    workspace.write("failing.toml", UNREACHABLE_CHAIN);
    let graph = graph_with(
        &scheduled_graph(
            &fake_harness(),
            &release.display().to_string(),
            "./oneharness.toml",
        )
        .replace(
            "members:\n",
            "members:\n  gate:\n    kind: oneharness\n    oneharness_config: ./failing.toml\n",
        ),
        &[
            // A second dependency on `report`, appended to the list the skeleton
            // already gave it.
            ("members.report.deps.1", "gate".to_string()),
            (UNREACHABLE_HARNESS_KEY, workspace.unreachable_harness()),
        ],
    );
    workspace.graph(&graph);
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: independent failure",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    let events_path = || {
        std::fs::read_dir(workspace.state())
            .into_iter()
            .flatten()
            .flatten()
            .next()
            .map(|entry| entry.path().join("events.jsonl"))
    };
    until("the failed gate and live keeper", || {
        events_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|stream| {
                stream.contains("\"member\":\"keeper\"")
                    && stream.lines().any(|line| {
                        line.contains("\"kind\":\"member-died\"")
                            && line.contains("\"member\":\"gate\"")
                    })
            })
    });
    let id = workspace.record()["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    workspace.run(&["trigger", &id, "ticker"]).expect_code(0);
    until("the triggered ticker to settle", || {
        events_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|stream| {
                stream.contains("\"kind\":\"cron-fired\"")
                    && stream
                        .lines()
                        .filter(|line| {
                            line.contains("\"kind\":\"member-settled\"")
                                && line.contains("\"member\":\"ticker\"")
                        })
                        .count()
                        >= 2
            })
    });
    let stream = std::fs::read_to_string(events_path().expect("events")).expect("stream");
    assert!(
        stream.lines().all(|line| {
            !line.contains("\"kind\":\"member-started\"") || !line.contains("\"member\":\"report\"")
        }),
        "the cron iteration ignored its failed independent dependency: {stream}"
    );
    std::fs::write(&release, "release").expect("release keeper");
    let output = child.wait_with_output().expect("run finishes");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn cron_iterations_observe_independent_dependencies_settled_in_later_waves() {
    let workspace = Workspace::new();
    let release = workspace.at("later-success-release");
    let graph = scheduled_graph(
        &fake_harness(),
        &release.display().to_string(),
        "./oneharness.toml",
    )
    .replace(
        "  keeper:\n",
        "  prerequisite:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n    deps: [bridge]\n  keeper:\n",
    )
    .replace("mode: bypass\n    deps: [bridge]", "mode: bypass\n    deps: [prerequisite]")
    .replace("deps: [ticker]", "deps: [ticker, prerequisite]");
    workspace.graph(&graph);
    let child = workspace.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: later independent success",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[],
    );
    let events_path = || {
        std::fs::read_dir(workspace.state())
            .into_iter()
            .flatten()
            .flatten()
            .next()
            .map(|entry| entry.path().join("events.jsonl"))
    };
    until(
        "the later prerequisite and initial report to settle",
        || {
            events_path()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .is_some_and(|stream| {
                    ["prerequisite", "report"].iter().all(|member| {
                        stream.lines().any(|line| {
                            line.contains("\"kind\":\"member-settled\"")
                                && line.contains(&format!("\"member\":\"{member}\""))
                        })
                    })
                })
        },
    );
    let id = workspace.record()["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    workspace.run(&["trigger", &id, "ticker"]).expect_code(0);
    until("the cron chain to use the later success", || {
        events_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|stream| {
                stream
                    .lines()
                    .filter(|line| {
                        line.contains("\"kind\":\"member-started\"")
                            && line.contains("\"member\":\"report\"")
                    })
                    .count()
                    >= 2
            })
    });
    std::fs::write(&release, "release").expect("release keeper");
    let output = child.wait_with_output().expect("run finishes");
    assert_eq!(output.status.code(), Some(0));
}
