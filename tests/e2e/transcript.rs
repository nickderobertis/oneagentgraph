//! Durable transcript references through real graph runs and history files.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

use crate::support::{fake_harness, single_sided_graph, two_party_graph, Workspace, NO_ENV};

fn path(reference: &Value) -> PathBuf {
    Path::new(reference["history_dir"].as_str().unwrap())
        .join(reference["history_project"].as_str().unwrap())
        .join(format!(
            "{}.jsonl",
            reference["history_session"].as_str().unwrap()
        ))
}

pub(super) fn entry(event: &Value) -> Value {
    let reference = &event["payload"]["transcript"];
    let activity = event["kind"] == "turn-activity";
    let kind = match event["kind"].as_str().unwrap() {
        "turn-started" => "run-started",
        "turn-message" => "run",
        "turn-activity" => "event",
        _ => panic!("not a content record"),
    };
    std::fs::read_to_string(path(reference))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|line| {
            line["type"] == kind
                && line[if activity { "run_id" } else { "history_id" }] == reference["history_id"]
                && (!activity || line["event"]["index"] == reference["index"])
        })
        .unwrap_or_else(|| panic!("unresolved {event}"))
}

fn no_copy(event: &Value) {
    assert!(event["payload"]["transcript"].is_object(), "{event}");
    for field in [
        "instruction",
        "instruction_truncated",
        "text",
        "truncated",
        "detail",
        "output",
        "output_truncated",
    ] {
        assert!(event["payload"].get(field).is_none(), "{field}: {event}");
    }
}

#[test]
fn live_references_resolve_whole_content_and_survive_cleanup_for_both_members() {
    for graph in [
        two_party_graph(&fake_harness(), NO_ENV),
        single_sided_graph(&fake_harness()),
    ] {
        let workspace = Workspace::new();
        workspace.graph(&graph);
        let history = workspace.at("durable-history");
        let release = workspace.at("release");
        let answer = workspace.at("answer");
        let reply = "whole assistant reply\n".repeat(300);
        std::fs::write(&answer, &reply).unwrap();
        let task = format!(
            "fake:complete-now fake:activity-hold={} fake:answer-file={} {}",
            release.display(),
            answer.display(),
            "whole instruction ".repeat(400)
        );
        let history_string = history.display().to_string();
        let task_file = workspace.at("task");
        std::fs::write(&task_file, &task).unwrap();
        let mut child = workspace.spawn_with(
            &[
                "run",
                "graph.yaml",
                "--task-file",
                task_file.to_str().unwrap(),
                "--dir",
                workspace.dir().to_str().unwrap(),
            ],
            &[
                ("ONEHARNESS_HISTORY", "1"),
                ("ONEHARNESS_HISTORY_DIR", &history_string),
            ],
        );
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                sender
                    .send(serde_json::from_str::<Value>(&line.unwrap()).unwrap())
                    .unwrap();
            }
        });
        let mut events = Vec::new();
        let live = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            loop {
                let event = receiver
                    .recv_timeout(Duration::from_secs(30))
                    .expect("live activity");
                let first_activity = event["kind"] == "turn-activity";
                events.push(event);
                if first_activity {
                    break;
                }
            }
            assert!(child.try_wait().unwrap().is_none(), "harness was not held");
            for event in events.iter().filter(|event| {
                matches!(
                    event["kind"].as_str(),
                    Some("turn-started" | "turn-activity")
                )
            }) {
                no_copy(event);
                let resolved = entry(event);
                if event["kind"] == "turn-started" {
                    assert_eq!(resolved["prompt"], task);
                } else {
                    assert_eq!(
                        resolved["event"]["input"],
                        serde_json::json!({"command":"just check"})
                    );
                }
            }
        }));
        // Always release our child before any later assertion can fail.
        std::fs::write(&release, "go").unwrap();
        if let Err(panic) = live {
            let _ = child.wait();
            let _ = reader.join();
            std::panic::resume_unwind(panic);
        }
        let status = child.wait().unwrap();
        reader.join().unwrap();
        events.extend(receiver);
        assert!(status.success(), "{events:?}");
        for event in &events {
            if !matches!(
                event["kind"].as_str(),
                Some("turn-started" | "turn-message" | "turn-activity")
            ) {
                continue;
            }
            no_copy(event);
            let record = entry(event);
            match event["kind"].as_str().unwrap() {
                "turn-started" if event["payload"]["role"] == "assistant" => {
                    assert_eq!(record["prompt"], task)
                }
                "turn-message" if event["payload"]["role"] == "assistant" => {
                    assert_eq!(record["text"], reply.trim_end())
                }
                "turn-activity" if event["payload"]["kind"] == "tool_result" => assert_eq!(
                    record["event"]["output"],
                    "running the gate\n2 passed; 0 failed"
                ),
                _ => (),
            }
            if event["kind"] == "turn-activity" {
                let candidate = &event["payload"]["transcript"]["history_id"];
                let sessions: Vec<_> = events
                    .iter()
                    .filter(|item| {
                        item["kind"] == "oneharness-session"
                            && item["payload"]["history_id"] == *candidate
                    })
                    .collect();
                assert_eq!(sessions.len(), 1);
                assert!(sessions[0]["seq"].as_u64() < event["seq"].as_u64());
            }
        }
        std::fs::remove_dir_all(workspace.dir()).unwrap();
        let scratch = workspace
            .state()
            .join(events[0]["labels"]["run_id"].as_str().unwrap())
            .join("members");
        assert!(scratch.exists(), "{}", scratch.display());
        std::fs::remove_dir_all(&scratch).unwrap();
        assert!(!scratch.exists());
        for event in events.iter().filter(|event| {
            matches!(
                event["kind"].as_str(),
                Some("turn-started" | "turn-message" | "turn-activity")
            )
        }) {
            entry(event);
        }
    }
}

#[test]
fn history_disabled_and_unopenable_stores_keep_bounded_copies() {
    for single in [false, true] {
        for unavailable in [false, true] {
            let workspace = Workspace::new();
            workspace.graph(&if single {
                single_sided_graph(&fake_harness())
            } else {
                two_party_graph(&fake_harness(), NO_ENV)
            });
            let store = workspace.at("blocked-store");
            std::fs::write(&store, "a file cannot be a history directory").unwrap();
            let config = format!(
                "{}history = {}\nhistory_dir = {:?}\n",
                crate::support::CHAIN,
                unavailable,
                store.to_str().unwrap()
            );
            workspace.write("oneharness.toml", &config);
            workspace.write("oneharness.judge.toml", &config);
            let task = format!("fake:complete-now {}", "instruction ".repeat(500));
            let run = workspace.run_task(&task);
            run.expect_code(0);
            assert!(run.of_kind("oneharness-session").is_empty());
            let started = run.of_kind("turn-started");
            assert!(!started.is_empty());
            for event in run.events().iter().filter(|event| {
                matches!(
                    event["kind"].as_str(),
                    Some("turn-started" | "turn-message" | "turn-activity")
                )
            }) {
                assert!(event["payload"].get("transcript").is_none(), "{event}");
                match event["kind"].as_str().unwrap() {
                    "turn-started" => {
                        let instruction = event["payload"]["instruction"].as_str().unwrap();
                        if event["payload"]["role"] == "assistant" {
                            assert_eq!(instruction, &task[..4096]);
                            assert_eq!(event["payload"]["instruction_truncated"], true);
                        } else {
                            assert_eq!(instruction, "done");
                            assert!(event["payload"].get("instruction_truncated").is_none());
                        }
                    }
                    "turn-message" => assert!(event["payload"]["text"].is_string()),
                    _ => assert!(event["payload"]["detail"].is_string()),
                }
            }
        }
    }
}

#[test]
fn request_refused_before_startup_keeps_the_bounded_instruction() {
    let workspace = Workspace::new();
    workspace.graph(&single_sided_graph(&fake_harness()));
    workspace.write("oneharness.toml", "harnesses = [\"not-a-harness\"]\n");
    let task = "instruction ".repeat(500);
    let run = workspace.run_task(&task);
    run.expect_code(1);
    assert!(run.of_kind("oneharness-session").is_empty());
    let starts = run.of_kind("turn-started");
    assert_eq!(starts.len(), 1, "{}", run.stdout);
    assert_eq!(starts[0]["payload"]["instruction"], &task[..4096]);
    assert_eq!(starts[0]["payload"]["instruction_truncated"], true);
    assert!(starts[0]["payload"].get("transcript").is_none());
}

#[test]
fn text_view_resolves_complete_instruction_reply_and_tool_output() {
    let workspace = Workspace::new();
    workspace.write(
        "oneharness.toml",
        &format!("{}history = true\n", crate::support::CHAIN),
    );
    workspace.write(
        "oneharness.judge.toml",
        &format!("{}history = true\n", crate::support::CHAIN),
    );
    let reply = "complete assistant text\n".repeat(300);
    let answer = workspace.at("answer");
    std::fs::write(&answer, &reply).unwrap();
    let task = format!(
        "fake:complete-now fake:noisy-tool fake:answer-file={} {}",
        answer.display(),
        "instruction ".repeat(500)
    );
    let run = workspace.run(&[
        "run",
        "graph.yaml",
        "--task",
        &task,
        "--dir",
        workspace.dir().to_str().unwrap(),
        "--output",
        "text",
    ]);
    run.expect_code(0);
    assert!(run.stdout.contains(&task), "instruction was cut");
    assert!(run.stdout.contains(reply.trim_end()), "reply was cut");
    assert!(run.stdout.contains("just check"));
    assert!(run.stdout.contains("running the gate\n2 passed; 0 failed"));
    assert!(
        !run.stdout.contains("transcript unavailable:"),
        "{}",
        run.stdout
    );
}

#[test]
fn sweep_keeps_aged_referenced_history_inside_its_state_root() {
    let workspace = Workspace::new();
    let store = workspace.state().join("oneharness/history");
    let run = workspace.run_task_with(
        "fake:complete-now",
        &[
            ("ONEHARNESS_HISTORY", "1"),
            ("ONEHARNESS_HISTORY_DIR", store.to_str().unwrap()),
        ],
    );
    run.expect_code(0);
    let event = run.of_kind("turn-started").remove(0);
    let file = path(&event["payload"]["transcript"]);
    let handle = std::fs::File::options().write(true).open(&file).unwrap();
    handle
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    let before = std::fs::read(&file).unwrap();
    let temp = workspace.at("sweep-temp");
    std::fs::create_dir_all(&temp).unwrap();
    workspace
        .run_with(
            &["sweep", "--min-age-hours", "1"],
            &[("TMPDIR", temp.to_str().unwrap())],
        )
        .expect_code(0);
    assert_eq!(std::fs::read(&file).unwrap(), before);
    entry(&event);
}

#[test]
fn command_and_llmlint_judge_messages_keep_their_bounded_copy() {
    for command in [true, false] {
        let workspace = Workspace::new();
        workspace.write("llmlint.yml", "rules: []\n");
        let judge = if command {
            format!(
                "command: [{}]",
                serde_json::to_string(&crate::support::fake_provider()).unwrap()
            )
        } else {
            format!(
                "kind: llmlint\n      config: ./llmlint.yml\n      bin: {}",
                serde_json::to_string(&crate::support::fake_llmlint()).unwrap()
            )
        };
        let graph = two_party_graph(&fake_harness(), NO_ENV)
            .replace("oneharness_config: ./oneharness.judge.toml", &judge);
        let marker = workspace.at("lint-ran");
        workspace.graph(&graph);
        let task = if command {
            "fake:should-fail"
        } else {
            "fake:complete-now"
        };
        let run = workspace.run_task_with(
            task,
            &[
                ("FAKE_LLMLINT_VERDICT", "fail-once"),
                ("FAKE_LLMLINT_MARKER", marker.to_str().unwrap()),
            ],
        );
        run.expect_code(if command { 1 } else { 0 });
        let messages: Vec<_> = run
            .of_kind("turn-message")
            .into_iter()
            .filter(|event| event["payload"]["role"] == "user")
            .collect();
        assert!(!messages.is_empty(), "{}\n{}", run.stdout, run.stderr);
        for message in messages {
            assert!(message["payload"].get("transcript").is_none(), "{message}");
            let text = message["payload"]["text"].as_str().unwrap();
            assert!(!text.is_empty() && text.len() <= 4096, "{message}");
        }
    }
}

/// A real consumer process drives the public event view over journal bytes a
/// real run emitted; the production CLI's live text mode is exercised above.
fn rendered(event: &Value) -> String {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_oneagentgraph-history-reader"))
        .arg("--render-events")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{event}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn rendered_views_report_missing_transcript_files_and_entries() {
    let workspace = Workspace::new();
    let run = workspace.run_task("fake:complete-now: read the whole transcript");
    run.expect_code(0);
    for event in run.events().into_iter().filter(|event| {
        matches!(
            event["kind"].as_str(),
            Some("turn-started" | "turn-message" | "turn-activity")
        )
    }) {
        let file = path(&event["payload"]["transcript"]);
        let original = std::fs::read(&file).unwrap();
        assert!(!rendered(&event).contains("transcript unavailable:"));
        std::fs::remove_file(&file).unwrap();
        let missing = rendered(&event);
        assert!(
            missing.contains(&format!("transcript unavailable: {}: ", file.display())),
            "{missing}"
        );
        assert!(
            missing.contains("No such file") || missing.contains("cannot find"),
            "{missing}"
        );
        // A readable session whose candidate entry was lost is a different
        // failure from a file that cannot be opened.
        std::fs::write(&file, b"").unwrap();
        let missing = rendered(&event);
        assert!(
            missing.contains(&format!(
                "transcript unavailable: {}: entry missing for ",
                file.display()
            )),
            "{missing}"
        );
        std::fs::write(&file, original).unwrap();
        assert!(!rendered(&event).contains("transcript unavailable:"));
    }
}

#[test]
fn rendered_views_refuse_malformed_references_and_history_content() {
    let workspace = Workspace::new();
    let run = workspace.run_task("fake:complete-now");
    run.expect_code(0);
    let started = run.of_kind("turn-started").remove(0);
    for reference in [
        serde_json::json!("not an address"),
        serde_json::json!({}),
        {
            let mut reference = started["payload"]["transcript"].clone();
            reference["history_project"] = serde_json::json!("../outside");
            reference
        },
    ] {
        let mut event = started.clone();
        event["payload"]["transcript"] = reference;
        assert!(rendered(&event).contains("transcript unavailable:"));
    }
    let activity = run.of_kind("turn-activity").remove(0);
    let mut missing_index = activity.clone();
    missing_index["payload"]["transcript"]
        .as_object_mut()
        .unwrap()
        .remove("index");
    assert!(rendered(&missing_index).contains("activity reference has no index"));
    for event in [started, activity, run.of_kind("turn-message").remove(0)] {
        let file = path(&event["payload"]["transcript"]);
        let original = std::fs::read(&file).unwrap();
        std::fs::write(&file, b"not-json\n").unwrap();
        assert!(rendered(&event).contains("transcript unavailable:"));
        let mut line = entry_from_bytes(&original, &event);
        let (field, diagnostic) = match event["kind"].as_str().unwrap() {
            "turn-started" => ("prompt", "run-started prompt is not a string"),
            "turn-message" => ("text", "run text is not a string"),
            _ => ("index", "event index is not an integer"),
        };
        if event["kind"] == "turn-activity" {
            line["event"].as_object_mut().unwrap().remove(field);
        } else {
            line.as_object_mut().unwrap().remove(field);
        }
        std::fs::write(&file, format!("{line}\n")).unwrap();
        assert!(
            rendered(&event).contains(diagnostic),
            "{}",
            rendered(&event)
        );
        if event["kind"] != "turn-activity" {
            line[field] = serde_json::json!({"wrong":"type"});
            std::fs::write(&file, format!("{line}\n")).unwrap();
            assert!(rendered(&event).contains(diagnostic));
        }
        if event["kind"] == "turn-activity" {
            let mut invalid_output = entry_from_bytes(&original, &event);
            invalid_output["event"]["output"] = serde_json::json!({"wrong":"type"});
            std::fs::write(&file, format!("{invalid_output}\n")).unwrap();
            assert!(rendered(&event).contains("event output is not a string"));
        }
        std::fs::write(&file, original).unwrap();
        assert!(!rendered(&event).contains("transcript unavailable:"));
    }
}

fn entry_from_bytes(bytes: &[u8], event: &Value) -> Value {
    let reference = &event["payload"]["transcript"];
    let activity = event["kind"] == "turn-activity";
    let kind = match event["kind"].as_str().unwrap() {
        "turn-started" => "run-started",
        "turn-message" => "run",
        _ => "event",
    };
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|line| {
            line["type"] == kind
                && line[if activity { "run_id" } else { "history_id" }] == reference["history_id"]
                && (!activity || line["event"]["index"] == reference["index"])
        })
        .unwrap()
}

#[test]
fn fallback_candidates_keep_distinct_references_when_indices_restart() {
    for graph in [
        two_party_graph(&fake_harness(), NO_ENV),
        single_sided_graph(&fake_harness()),
    ] {
        let workspace = Workspace::new();
        workspace.graph(&graph);
        workspace.write("oneharness.toml", &format!("run_mode = \"fallback\"\nhistory = true\nharnesses = [\"claude-code\", \"claude-code:alternate\"]\n[harness.claude-code.variant.alternate]\nbin = {:?}\n", fake_harness()));
        let marker = workspace.at("refused");
        let run = workspace.run_task(&format!(
            "fake:complete-now fake:activity-fallback={}",
            marker.display()
        ));
        run.expect_code(0);
        let firsts: Vec<_> = run
            .of_kind("turn-activity")
            .into_iter()
            .filter(|event| event["payload"]["index"] == 0)
            .collect();
        assert_eq!(firsts.len(), 2, "{}\n{}", run.stdout, run.stderr);
        assert_ne!(
            firsts[0]["payload"]["transcript"]["history_id"],
            firsts[1]["payload"]["transcript"]["history_id"]
        );
        assert_eq!(
            path(&firsts[0]["payload"]["transcript"]),
            path(&firsts[1]["payload"]["transcript"])
        );
        assert_eq!(
            entry(&firsts[0])["event"]["output"],
            "Considering the request before quota refused it."
        );
        assert_eq!(
            entry(&firsts[1])["event"]["input"],
            serde_json::json!({"command":"just check"})
        );
        let opening = run.of_kind("turn-started").remove(0);
        assert_eq!(
            opening["payload"]["transcript"]["history_id"],
            firsts[0]["payload"]["transcript"]["history_id"]
        );
        for event in run.of_kind("turn-activity") {
            no_copy(&event);
            let resolved = entry(&event);
            assert_eq!(resolved["event"]["index"], event["payload"]["index"]);
            let candidate = &event["payload"]["transcript"]["history_id"];
            let sessions: Vec<_> = run
                .of_kind("oneharness-session")
                .into_iter()
                .filter(|item| item["payload"]["history_id"] == *candidate)
                .collect();
            assert_eq!(sessions.len(), 1);
            assert!(sessions[0]["seq"].as_u64() < event["seq"].as_u64());
        }
    }
}

#[test]
fn stopped_single_sided_candidate_keeps_its_session_and_live_opening() {
    let workspace = Workspace::new();
    workspace.graph(&single_sided_graph(&fake_harness()));
    workspace.write(
        "oneharness.toml",
        &format!("{}history = true\n", crate::support::CHAIN),
    );
    let release = workspace.at("release");
    let task = format!("fake:complete-now fake:activity-hold={}", release.display());
    let mut child = workspace.spawn_with(
        &[
            "run",
            "graph.yaml",
            "--task",
            &task,
            "--dir",
            workspace.dir().to_str().unwrap(),
        ],
        &[],
    );
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            sender
                .send(serde_json::from_str::<Value>(&line.unwrap()).unwrap())
                .unwrap();
        }
    });
    let mut events = Vec::new();
    let live = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        loop {
            let event = receiver
                .recv_timeout(Duration::from_secs(30))
                .expect("held candidate activity");
            let activity = event["kind"] == "turn-activity";
            events.push(event);
            if activity {
                break;
            }
        }
        let opening = events
            .iter()
            .find(|event| event["kind"] == "turn-started")
            .unwrap();
        assert_eq!(entry(opening)["prompt"], task);
        assert!(child.try_wait().unwrap().is_none());
    }));
    if let Err(panic) = live {
        std::fs::write(&release, "release after failed assertion").unwrap();
        let _ = child.wait();
        let _ = reader.join();
        std::panic::resume_unwind(panic);
    }
    let id = events[0]["labels"]["run_id"].as_str().unwrap();
    let cancelled = workspace.run(&["cancel", id, "reporter", "--kill"]);
    std::fs::write(&release, "release if cancellation failed").unwrap();
    let status = child.wait().unwrap();
    reader.join().unwrap();
    events.extend(receiver);
    cancelled.expect_code(0);
    assert!(!status.success(), "cancelled member settled successfully");
    let sessions: Vec<_> = events
        .iter()
        .filter(|event| event["kind"] == "oneharness-session")
        .collect();
    assert_eq!(sessions.len(), 1);
    let opening = events
        .iter()
        .find(|event| event["kind"] == "turn-started")
        .unwrap();
    assert_eq!(
        sessions[0]["payload"]["history_id"],
        opening["payload"]["transcript"]["history_id"]
    );
    assert_eq!(entry(opening)["prompt"], task);
    let activity = events
        .iter()
        .find(|event| event["kind"] == "turn-activity")
        .unwrap();
    assert!(sessions[0]["seq"].as_u64() < activity["seq"].as_u64());
    entry(activity);
}
