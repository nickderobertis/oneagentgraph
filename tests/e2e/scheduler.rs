//! Scheduler journeys against the compiled binary and real oneharness process.

use std::time::{Duration, Instant};

use crate::support::{fake_harness, until, Workspace};

fn scheduled_graph(fake: &str, hold: &str, ticker_config: &str) -> String {
    format!(
        concat!(
            "version: 1\nname: scheduled-chain\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n",
            "  anchor:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  ticker:\n    kind: oneharness\n    oneharness_config: {ticker_config}\n",
            "    schedule: {{every: 3600}}\n",
            "  keeper:\n    kind: onejudge\n    base_config: ./base.yaml\n",
            "    persona: engineer\n",
            "    task: 'fake:complete-now fake:hold={hold}'\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n    deps: [anchor]\n",
            "  report:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    deps: [ticker]\n",
        ),
        fake = fake,
        hold = hold,
        ticker_config = ticker_config,
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
fn a_failed_cron_firing_never_starts_that_iterations_chain() {
    let workspace = Workspace::new();
    workspace.write(
        "failing.toml",
        "run_mode = \"fallback\"\nharnesses = [\"codex\"]\n",
    );
    workspace.graph(
        "version: 1\nname: failed-cron\nmembers:\n  ticker:\n    kind: oneharness\n    oneharness_config: ./failing.toml\n    schedule: {every: 3600}\n  report:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n    deps: [ticker]\n",
    );
    let run = workspace.run_task("fake:complete-now: scheduled");
    run.expect_code(1);
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
