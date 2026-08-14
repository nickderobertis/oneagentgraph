//! Scheduler journeys against the compiled binary and real oneharness process.

// llmlint: ignore-file[e2e_not_mocked] these journeys use the repository's sole
// sanctioned fake at oneharness's ONEHARNESS_BIN_<ID> paid-provider seam. The
// compiled oneagentgraph and real oneharness CLI/process boundary remain real.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::support::{fake_harness, graph_with, until, Workspace, FAKE_HARNESS_KEY};

/// The chain every journey below drives, whose `ticker` takes its first turn the
/// moment the graph starts.
///
/// `start_after: 0` says so out loud rather than relying on the default, which is
/// one whole interval: these journeys are about what a firing *does* — the chain
/// it runs, the failures it propagates, the quiescence that ends it — and each one
/// needs a firing to have happened. The default's own journeys are
/// [`a_deferred_schedule_starts_with_the_graph_and_takes_no_turn`] and
/// [`a_deferred_schedules_first_turn_waits_for_the_delay_it_named`].
fn scheduled_graph(fake: &str, hold: &str, ticker_config: &str) -> String {
    graph_with(
        concat!(
            "version: 2\nname: scheduled-chain\n",
            "env: {}\n",
            "members:\n",
            "  anchor:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  ticker:\n    kind: oneharness\n",
            "    schedule: {every: 3600, start_after: 0}\n",
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
/// `hold` is released to end the run. `start_after` is written as given, so the
/// two journeys below differ only in how long the deferred member waits.
///
/// The delay is substituted into the skeleton rather than passed through
/// [`graph_with`], which writes every value as a string: a schedule's seconds are
/// a number, and one quoted into the document would be refused by the schema
/// before either journey reached what it tests.
fn deferred_graph(fake: &str, hold: &str, start_after: u64, recorded: &str) -> String {
    const SKELETON: &str = concat!(
        "version: 3\nname: paced\n",
        "env: {}\n",
        "members:\n",
        "  worker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
        "  ticker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
        "    persona: reviewer\n",
        "    schedule: {every: 3600, start_after: 0}\n",
    );
    graph_with(
        &SKELETON.replace("start_after: 0", &format!("start_after: {start_after}")),
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
        3600,
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
    let events = || {
        std::fs::read_dir(workspace.state())
            .into_iter()
            .flatten()
            .flatten()
            .next()
            .map(|entry| entry.path().join("events.jsonl"))
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default()
    };
    until("both members to start", || {
        ["worker", "ticker"].iter().all(|member| {
            events().lines().any(|line| {
                line.contains("\"kind\":\"member-started\"")
                    && line.contains(&format!("\"member\":\"{member}\""))
            })
        })
    });

    // Started: the deferred member's own `member-started`, carrying the launch it
    // will run and the delay before it runs it.
    let stream = events();
    let started: Value = stream
        .lines()
        .filter(|line| {
            line.contains("\"kind\":\"member-started\"") && line.contains("\"member\":\"ticker\"")
        })
        .map(|line| serde_json::from_str(line).expect("an envelope"))
        .next()
        .expect("the deferred member started");
    assert_eq!(started["payload"]["start_after"], 3600);
    assert_eq!(started["payload"]["runner"], "process");
    assert_eq!(
        crate::support::labels(&started).get("persona").cloned(),
        Some("reviewer".to_string()),
        "a deferred member that came up must carry the persona it resolved: {started}"
    );
    // Its configuration is not a promise: the generated oneharness config its
    // argv names is on disk, which is the work a bad ref would have failed at.
    let config = started["payload"]["args"]
        .as_array()
        .and_then(|args| {
            args.iter()
                .position(|arg| arg == "--config")
                .and_then(|at| args.get(at + 1))
        })
        .and_then(Value::as_str)
        .expect("the argv names a config");
    assert!(
        std::path::Path::new(config).is_file(),
        "the deferred member's generated config was never written: {config}"
    );

    // And no turn: nothing fired, nothing was published for a turn, and the
    // harness that would have been paid for one never ran.
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
    no_turn(&stream);

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
    no_turn(&events());
}

/// A deferred schedule's first turn happens once its delay has elapsed, and not
/// before it.
///
/// The other half of the journey above, and what keeps it from being satisfied by
/// a member that simply never fires: the same graph with a short delay takes its
/// turn, runs the prompt it was given, and does so no sooner than the delay it
/// named — measured from before the run was even launched, so a firing at t=0
/// could not fit inside it.
#[test]
fn a_deferred_schedules_first_turn_waits_for_the_delay_it_named() {
    let workspace = Workspace::new();
    let release = workspace.at("paced-release");
    let recorded = workspace.at("ticker.prompt");
    workspace.graph(&deferred_graph(
        &fake_harness(),
        &release.display().to_string(),
        3,
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
    until("the deferred first turn", || recorded.is_file());
    let waited = launched.elapsed();
    assert!(
        waited >= Duration::from_secs(3),
        "the first turn came {waited:?} after launch, sooner than the delay it named"
    );
    assert!(
        std::fs::read_to_string(&recorded)
            .expect("the prompt the deferred member ran")
            .contains("report progress"),
        "the deferred member ran something other than its own task"
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
            "    schedule: {every: 3600, start_after: 0}\n",
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
    workspace.write(
        "failing.toml",
        "run_mode = \"fallback\"\nharnesses = [\"codex\"]\n",
    );
    workspace.graph(
        "version: 1\nname: failed-initial-cron\nmembers:\n  ticker:\n    kind: oneharness\n    oneharness_config: ./failing.toml\n    schedule: {every: 3600, start_after: 0}\n  report:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n    deps: [ticker]\n",
    );
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
    workspace.write(
        "failing.toml",
        "run_mode = \"fallback\"\nharnesses = [\"codex\"]\n",
    );
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
        // A second dependency on `report`, appended to the list the skeleton
        // already gave it.
        &[("members.report.deps.1", "gate")],
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
