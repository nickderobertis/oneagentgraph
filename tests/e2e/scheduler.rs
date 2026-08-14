//! Scheduler journeys against the compiled binary and real oneharness process.

// llmlint: ignore-file[e2e_not_mocked] these journeys use the repository's sole
// sanctioned fake at oneharness's ONEHARNESS_BIN_<ID> paid-provider seam. The
// compiled oneagentgraph and real oneharness CLI/process boundary remain real.

use std::time::{Duration, Instant};

use crate::support::{fake_harness, graph_with, until, Workspace, FAKE_HARNESS_KEY};

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
    workspace.write(
        "failing.toml",
        "run_mode = \"fallback\"\nharnesses = [\"codex\"]\n",
    );
    workspace.graph(
        "version: 1\nname: failed-initial-cron\nmembers:\n  ticker:\n    kind: oneharness\n    oneharness_config: ./failing.toml\n    schedule: {every: 3600}\n  report:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n    deps: [ticker]\n",
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
