//! Dispatch journeys, ported from `ai-orchestrator/tests/e2e/test_dispatch_e2e.py`.
//!
//! Each drives the real `oneagentgraph` binary against a real graph, through the
//! real `onejudge` loop and the real `oneharness` selection, with only the paid
//! harness process replaced at oneharness's own seam. What is asserted is the
//! stream: the contract says exit 1 means "a member failed or died (the stream
//! says which and why)", so a journey that only checked the code would leave the
//! part a supervisor reads unproven.

// llmlint: ignore-file[e2e_not_mocked] see tests/e2e/support.rs: the paid harness
// process is the single sanctioned double, replaced at oneharness's own
// `ONEHARNESS_BIN_<ID>` seam, with real onejudge and real oneharness in between.

use crate::support::{
    fake_harness, fake_provider, labels, single_sided_graph, two_party_graph, Workspace, CHAIN,
};

/// The whole happy path: a two-party member completes, and the stream carries
/// the lifecycle a consumer renders.
///
/// Ported from `test_dispatch_completes_via_supervisor_loop` and
/// `test_dispatch_complete_now_single_turn`.
#[test]
fn a_member_completes_through_the_real_supervisor_loop() {
    let workspace = Workspace::new();
    let run = workspace.run_task("fake:complete-now: write the thing");
    run.expect_code(0);

    let kinds = run.kinds();
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
            kinds.iter().any(|kind| kind == expected),
            "the stream never carried {expected}: {kinds:?}"
        );
    }
    assert_eq!(kinds.first().map(String::as_str), Some("graph-started"));
    assert_eq!(kinds.last().map(String::as_str), Some("graph-settled"));

    // The turn events a two-party member produces are built from onejudge's
    // *typed* live sink now, so what they carry is asserted here rather than
    // only that they arrived. This is the CLI half of the conversion: every
    // field the engine hands this process in-library has to be reachable by a
    // caller who only has the stream, and machine-readable when it gets there.
    let activity = run.of_kind("turn-activity");
    assert!(!activity.is_empty(), "{kinds:?}");
    for event in &activity {
        let payload = &event["payload"];
        for named in ["kind", "name", "detail"] {
            assert!(
                payload[named].is_string(),
                "turn-activity carried no {named}: {payload}"
            );
        }
        assert!(
            payload["name"]
                .as_str()
                .is_some_and(|name| !name.is_empty()),
            "an action this crate could not name was published anyway: {payload}"
        );
        // The contract's bound on a tool summary, on the wire where a consumer
        // meets it.
        assert!(
            payload["detail"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .count()
                <= 160,
            "the tool detail outgrew its documented bound: {payload}"
        );
        assert!(payload["truncated"].is_boolean(), "{payload}");
    }
    for event in &run.of_kind("turn-started") {
        assert!(
            event["payload"]["turn"].as_u64().is_some(),
            "turn-started named no turn: {event}"
        );
    }
    // `turn-completed` carries the usage the contract names. It comes off the
    // report onejudge returns to this process, so a caller reading only the
    // stream still gets the accounting rather than having to open the artifact.
    let completed = run.of_kind("turn-completed");
    assert_eq!(completed.len(), 1, "{completed:?}");
    let usage = &completed[0]["payload"]["usage"];
    assert!(
        usage.is_object(),
        "turn-completed carried no usage: {usage}"
    );
    // Every field the contract names for this payload: tokens in and out, cache
    // reads and writes, and cost.
    for counted in [
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "cost_usd",
    ] {
        assert!(
            usage.get(counted).is_some_and(|value| value.is_number()),
            "the usage a consumer bills on has no {counted}: {usage}"
        );
    }

    // A two-party member says which runner it has, and what that runner is: the
    // engine driving it and the effective config it was given. There is no
    // `program` or `args` on this one, because nothing was spawned for it — and
    // a consumer reading the stream is how an operator learns that.
    let started = run.of_kind("member-started");
    assert_eq!(started.len(), 1, "{started:?}");
    let payload = &started[0]["payload"];
    assert_eq!(payload["runner"], serde_json::json!("library"));
    assert_eq!(payload["engine"], serde_json::json!("onejudge"));
    assert!(
        payload["config"]
            .as_str()
            .is_some_and(|config| config.ends_with("onejudge.yaml")),
        "the member named no effective config: {payload}"
    );
    assert!(
        payload["worktree"]
            .as_str()
            .is_some_and(|worktree| !worktree.is_empty()),
        "the member named no worktree: {payload}"
    );
    // Not `cwd`: a member driven in this process has no working directory of its
    // own, and claiming one would name a thing that is not true.
    for absent in ["program", "args", "cwd"] {
        assert!(
            payload.get(absent).is_none(),
            "a member nothing was spawned for reported {absent}: {payload}"
        );
    }

    let settled = run.of_kind("member-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["payload"]["completed"], serde_json::json!(true));
    // The full report is an artifact and the verdict is inline — the contract's
    // own split between what a stream carries and what it references.
    assert_eq!(
        settled[0]["artifacts"][0]["kind"],
        serde_json::json!("report")
    );
    assert!(settled[0]["artifacts"][0]["bytes"].as_u64().unwrap_or(0) > 0);
    assert!(settled[0]["payload"]["verdict"].is_array());
}

/// A single-sided member says the other thing: it is a child process, and it
/// names the program and argv it was spawned with.
///
/// The pair with the journey above is the point. `member-started` now carries a
/// `runner`, and a consumer branching on it has to be able to trust that the
/// fields beside it match — a member reported as a process with no argv, or as a
/// library call with one, is a stream that cannot be read.
#[test]
fn a_single_sided_member_reports_the_process_it_spawned() {
    let workspace = Workspace::new();
    workspace.graph(&single_sided_graph(&fake_harness()));
    let run = workspace.run_task("fake:complete-now: one sided");
    run.expect_code(0);

    let started = run.of_kind("member-started");
    assert_eq!(started.len(), 1, "{started:?}");
    let payload = &started[0]["payload"];
    assert_eq!(payload["runner"], serde_json::json!("process"));
    assert!(
        payload["program"]
            .as_str()
            .is_some_and(|program| program.contains("oneharness")),
        "the member named no program: {payload}"
    );
    assert!(
        payload["args"]
            .as_array()
            .is_some_and(|args| args.iter().any(|arg| arg == "run")),
        "the member named no argv: {payload}"
    );
    assert!(
        payload["cwd"].as_str().is_some_and(|cwd| !cwd.is_empty()),
        "the child named no working directory: {payload}"
    );
    assert!(payload.get("engine").is_none(), "{payload}");

    // And this member stores its report where the settle says it is, the same as
    // a two-party one: the artifact the contract promises has something behind
    // its id to fetch whichever kind of member produced it.
    let settled = run.of_kind("member-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    let path = settled[0]["payload"]["report_path"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!path.is_empty(), "the settle named no stored report");
    assert_eq!(
        settled[0]["artifacts"][0]["bytes"].as_u64().unwrap_or(0),
        std::fs::metadata(&path).expect("the stored report").len(),
        "the artifact's size is not the size of what was stored"
    );
}

/// A single-sided member whose harness exits without publishing a report dies as
/// a provider failure — and, because this member really *was* a process, its
/// death carries the three facts one leaves behind alongside the typed cause.
///
/// The pair with the two-party provider failure in `tests/e2e/liveness.rs` is the
/// point: one member kind has an exit status and a stderr tail and the other does
/// not, and `member-died` has to say which it is rather than reporting a process
/// that never existed.
#[test]
fn a_single_sided_member_that_crashes_carries_its_process_s_own_facts() {
    let workspace = Workspace::new();
    workspace.graph(&single_sided_graph(&fake_harness()));
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: the provider dies",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[("FAKE_HARNESS_CRASH", "1")],
    );
    run.expect_code(1);

    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{:?}", run.kinds());
    let payload = &died[0]["payload"];
    assert_eq!(payload["rule"], serde_json::json!("provider-failure"));
    // A process's own disposition, not a provider classification: an exit status
    // is what a process failure *is*, and inventing a category from one would be
    // this crate guessing at something oneharness already owns.
    assert_eq!(payload["cause"], serde_json::json!("exited"));
    assert_eq!(payload["disposition"], serde_json::json!("exited"));
    assert!(
        payload["exit_code"].as_i64().is_some(),
        "a child that exited reported no code: {payload}"
    );
    let tail = payload["stderr_tail"].as_str().unwrap_or_default();
    assert!(!tail.is_empty(), "a death with no evidence: {payload}");
    assert_eq!(
        payload["detail"], payload["stderr_tail"],
        "the detail every member carries must be this one's evidence too"
    );
    assert!(
        tail.len() <= 4096,
        "the stderr tail outgrew its documented bound"
    );
}

/// A base config that names its own **skill directory** loads it from where its
/// author wrote it — a path relative to that config, not to the copy this crate
/// generates.
///
/// onejudge resolves a config-file `skill:` against *that config's* directory,
/// and the config it is now handed is the merged copy written into the member's
/// scratch, a directory the author never saw. Left alone, `skills/greeter`
/// resolved under the scratch and the member died having found no `SKILL.md`.
/// The skill's body arriving as the harness's instructions is the proof it was
/// found, and finding it is the whole of what naming one does.
#[test]
fn a_base_config_that_names_a_skill_directory_loads_it_from_where_it_was_written() {
    let workspace = Workspace::new();
    workspace.write(
        "skills/greeter/SKILL.md",
        "---\nname: greeter\ndescription: a greeter\n---\nGreet the user warmly.\n",
    );
    // Relative, as an author writes it: against the base config's own directory,
    // which is this workspace.
    workspace.write(
        "base.yaml",
        &format!("{}skill: skills/greeter\n", crate::support::BASE),
    );
    let prompts = workspace.at("prompts.txt");
    workspace
        .run_task(&format!(
            "fake:complete-now: skill body fake:record-prompt={}",
            prompts.display()
        ))
        .expect_code(0);
    assert!(
        std::fs::read_to_string(&prompts)
            .expect("the harness recorded its prompt")
            .contains("Greet the user warmly."),
        "the skill the base config named never reached the harness"
    );
}

/// A graph carrying an `env` value the platform cannot represent is refused by
/// `validate`, before anything is started.
///
/// The block reaches *this* process's environment now, because a two-party member
/// inherits it from here rather than being handed one — and `set_var` answers a
/// value it cannot represent by panicking. A graph is a document somebody wrote,
/// so it gets exit 2 and a sentence, not a crash.
#[test]
fn a_graph_env_value_the_platform_cannot_represent_is_refused() {
    let workspace = Workspace::new();
    workspace.graph(concat!(
        "version: 1\nname: node-scope\n",
        "env:\n  ONEAGENTGRAPH_TEST_MARKER: \"has\\0nul\"\n",
        "members:\n  reporter:\n    kind: oneharness\n",
        "    oneharness_config: ./oneharness.toml\n",
    ));
    let validated = workspace.run(&["validate", "./graph.yaml"]);
    validated.expect_code(2);
    assert!(
        validated.stderr.contains("exported to every member"),
        "{}",
        validated.stderr
    );

    // And a run refuses it on the same terms rather than starting members under
    // a block it cannot apply.
    let run = workspace.run_task("fake:complete-now: never gets here");
    run.expect_code(2);
    assert!(
        run.stdout.is_empty(),
        "a refusal must not read as an event stream"
    );
}

/// A task that *talks about* the double's sentinels is steered by none of them.
///
/// This is the incident the `fake:` prefix exists for, made a test rather than a
/// comment: what the double matches on is the whole rendered prompt, persona and
/// task included, so a bare `hang` inside `change` once parked every turn of the
/// suite. Left unprefixed, `should-fail` here would make the supervisor refuse to
/// complete and this run would exit 1 at its turn cap; `hang` would park it until
/// a watchdog fired. Both words are in the prose, and neither is a sentinel.
#[test]
fn a_task_whose_prose_contains_a_bare_sentinel_is_not_steered_by_it() {
    let workspace = Workspace::new();
    let run = workspace
        .run_task("fake:complete-now: say whether a should-fail case can hang a change review");
    run.expect_code(0);

    let settled = run.of_kind("member-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["payload"]["completed"], serde_json::json!(true));
}

/// A member that never reaches its bar settles incomplete, and the run exits 1
/// with the stream saying which member and why.
///
/// Ported from `test_dispatch_hits_turn_cap_when_never_done` and
/// `test_run_onejudge_returns_incomplete_report_for_exit_one`.
#[test]
fn a_member_that_never_completes_exits_one_and_the_stream_says_which() {
    let workspace = Workspace::new();
    let run = workspace.run_task("fake:should-fail: never reach the bar");
    run.expect_code(1);

    let settled = run.of_kind("member-settled");
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0]["payload"]["completed"], serde_json::json!(false));
    assert_eq!(labels(&settled[0])["member"], "worker");
    assert_eq!(
        workspace.record()["members"]["worker"],
        serde_json::json!("incomplete")
    );
    assert_eq!(workspace.record()["exit_code"], serde_json::json!(1));
}

/// Every event carries the member and persona it came from, and the run id every
/// event in the run shares — the reserved labels a consumer joins on.
///
/// Ported from the label half of `test_real_dispatch_delivers_exact_task_to_agent_history`.
#[test]
fn every_event_carries_the_labels_a_consumer_joins_on() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: label me",
        "--dir",
        &workspace.dir().display().to_string(),
        "--label",
        "round=2",
        "--label",
        "node=service",
    ]);
    run.expect_code(0);

    let events = run.events();
    let run_id = labels(&events[0])["run_id"].clone();
    assert!(!run_id.is_empty());
    for event in &events {
        let stamped = labels(event);
        assert_eq!(stamped["run_id"], run_id, "{event}");
        assert_eq!(stamped["round"], "2", "{event}");
        assert_eq!(stamped["node"], "service", "{event}");
        assert_eq!(event["v"], serde_json::json!(1));
        assert_eq!(event["source"], serde_json::json!("agentgraph"));

        // `ts` is the first key of the documented merge order `(ts, stream,
        // seq)`, so its *shape* is load-bearing for a consumer joining two
        // producers: RFC 3339, millisecond, UTC — which is also what makes the
        // strings sort in time order. Asserted on the stream a consumer reads
        // rather than on the struct, because the wire is where it has to hold.
        let ts = event["ts"].as_str().unwrap_or_default();
        assert_eq!(ts.len(), 24, "not a millisecond RFC 3339 stamp: {event}");
        assert!(ts.ends_with('Z'), "a stamp that is not UTC: {event}");
        let (date, rest) = ts.split_at(10);
        assert!(
            date.split('-').count() == 3 && rest.starts_with('T'),
            "not an RFC 3339 date-time: {event}"
        );
        let millis = &ts[20..23];
        assert!(
            millis.chars().all(|digit| digit.is_ascii_digit()),
            "millisecond precision is not three digits: {event}"
        );
    }
    // And they sort in the order they were emitted, which is the property the
    // merge order rests on.
    let stamps: Vec<&str> = events
        .iter()
        .filter_map(|event| event["ts"].as_str())
        .collect();
    let mut ordered = stamps.clone();
    ordered.sort_unstable();
    assert_eq!(stamps, ordered, "the stamps do not sort in emission order");
    let member_events: Vec<_> = events
        .iter()
        .filter(|event| labels(event).contains_key("member"))
        .collect();
    assert!(!member_events.is_empty());
    for event in member_events {
        assert_eq!(labels(event)["member"], "worker");
        assert_eq!(labels(event)["persona"], "engineer", "{event}");
    }
}

/// `seq` is monotonic per stream with no gaps, which is how a consumer detects
/// loss.
#[test]
fn seq_is_monotonic_with_no_gaps() {
    let workspace = Workspace::new();
    let run = workspace.run_task("fake:complete-now: number me");
    run.expect_code(0);

    let events = run.events();
    let stream = events[0]["stream"].clone();
    for (expected, event) in events.iter().enumerate() {
        assert_eq!(event["seq"], serde_json::json!(expected as u64), "{event}");
        assert_eq!(event["stream"], stream, "{event}");
    }
}

// llmlint: ignore-block[tests_mirror_real_usage] the assertions in this block read a
// file the doubled harness wrote recording the prompt, environment, or argv it was
// given, and that is the observation point *because* it is the subject: what
// arrived at the far end of the invocation this crate builds. Nothing a user reads
// carries it — the stream reports that a member settled, not what the harness was
// asked, and a run that delivered a mangled task or spent the wrong subscription
// exits 0 with a stream identical to a correct one. Asserting only on the stream
// would leave the whole delivery path unproven, which is the path these journeys
// were ported to cover; ai-orchestrator's originals record at the same point for
// the same reason. The recorder is the single sanctioned double, and every
// assertion about behaviour — exit codes, event kinds, the run record — is still
// made through the CLI.
/// The task reaches the agent side exactly as it was given — including the
/// metacharacters a shell would have eaten.
///
/// Ported from `test_real_dispatch_delivers_exact_task_to_agent_history` and
/// `test_just_run_plan_preserves_metacharacter_laden_arguments`.
#[test]
fn the_exact_task_reaches_the_agent_side() {
    let workspace = Workspace::new();
    let record = workspace.at("prompts.txt");
    let task = format!(
        "fake:complete-now: $(touch /tmp/pwned) `id` \"quoted\" 'single' | & ; \\ \
         fake:record-prompt={}",
        record.display()
    );
    workspace.run_task(&task).expect_code(0);

    let delivered = std::fs::read_to_string(&record).expect("the agent recorded its prompt");
    assert!(
        delivered.contains("$(touch /tmp/pwned) `id` \"quoted\" 'single' | & ;"),
        "the task was mangled on its way to the agent: {delivered}"
    );
    assert!(
        !std::path::Path::new("/tmp/pwned").exists(),
        "the task was evaluated as a shell"
    );
}

/// The persona's role is appended after the base's shared preamble, and both
/// reach the agent — the merge the contract's `persona` field buys.
///
/// Ported from `test_dispatch_subdir_qualified_persona_via_real_onejudge`.
#[test]
fn the_base_preamble_and_the_persona_role_both_reach_the_agent() {
    let workspace = Workspace::new();
    workspace.write(
        "roles/lead.yaml",
        concat!(
            "agent:\n  name: lead\n  instructions: |\n    Role marker: you lead.\n",
            "user:\n  persona: |\n    Supervisor marker: push hard.\n",
        ),
    );
    workspace.graph(
        &two_party_graph(&fake_harness(), "")
            .replace("persona: engineer", "persona: ./roles/lead.yaml"),
    );

    let record = workspace.at("prompts.txt");
    let run = workspace.run_task(&format!(
        "fake:complete-now: merged fake:record-prompt={}",
        record.display()
    ));
    run.expect_code(0);

    let delivered = std::fs::read_to_string(&record).expect("prompts");
    assert!(
        delivered.contains("Standing bar: verify before you claim done."),
        "{delivered}"
    );
    assert!(delivered.contains("Role marker: you lead."), "{delivered}");
    assert!(
        delivered.contains("Supervisor marker: push hard."),
        "{delivered}"
    );
    // The persona's own name is what the events are labelled with.
    assert_eq!(labels(&run.of_kind("member-started")[0])["persona"], "lead");
}

/// A graph's `env` reaches every member process, `${VAR}` expanded — and that is
/// the seam the whole suite reaches the double through, so it is proven rather
/// than assumed.
///
/// Ported from `test_dispatch_forwards_validated_environment_to_real_provider`.
#[test]
fn the_graphs_env_reaches_the_member_process_expanded() {
    let workspace = Workspace::new();
    let marker = workspace.at("marker");
    workspace.graph(&two_party_graph(
        &fake_harness(),
        "  ONEAGENTGRAPH_TEST_MARKER: ${E2E_SOURCE}/leaf\n",
    ));
    let record = workspace.at("env.txt");
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            &format!(
                "fake:complete-now: env fake:record-env={}",
                record.display()
            ),
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[("E2E_SOURCE", &marker.display().to_string())],
    );
    run.expect_code(0);

    let recorded = std::fs::read_to_string(&record).expect("env");
    let first: serde_json::Value =
        serde_json::from_str(recorded.lines().next().expect("a line")).expect("json");
    assert_eq!(
        first["ONEAGENTGRAPH_TEST_MARKER"],
        serde_json::json!(format!("{}/leaf", marker.display())),
        "the graph's env did not reach the member expanded: {recorded}"
    );
}

/// A member's `mode` reaches both sides as the approval posture, because
/// onejudge's own config schema has no key for it.
///
/// Asserted on the **argv oneharness built**, which is oneharness applying the
/// mode rather than merely being told it: `read-only` is the one posture that
/// denies the mutating tools. That is also what makes this journey the proof
/// that a per-member setting still arrives without being exported — every
/// two-party member of every graph now runs in one process, so `mode` rides each
/// side's own resolved config instead of the environment, and a member whose
/// stamp went missing would spawn a harness with no `--disallowedTools` at all.
#[test]
fn the_members_mode_reaches_the_harness_process() {
    let workspace = Workspace::new();
    let record = workspace.at("argv.txt");
    workspace
        .graph(&two_party_graph(&fake_harness(), "").replace("mode: bypass", "mode: read-only"));
    workspace
        .run_task(&format!(
            "fake:complete-now: mode fake:record-argv={}",
            record.display()
        ))
        .expect_code(0);

    let recorded = std::fs::read_to_string(&record).expect("the harness recorded its argv");
    let lines: Vec<&str> = recorded.lines().collect();
    assert!(
        lines.len() >= 2,
        "not every side reached the double: {recorded}"
    );
    for line in &lines {
        // oneharness's own spelling of the posture, and it is oneharness's to
        // change: a read-only claude-code turn is now the *allowlist*
        // `--tools <the tools that only read>` rather than a denylist. What this
        // journey holds is that the member's `mode` reached the process at all,
        // so the assertion tracks the spelling rather than pinning a flag name
        // this crate does not own.
        assert!(
            line.contains("--tools Read Grep Glob"),
            "a side ran without the member's read-only posture: {line}"
        );
    }
}

// llmlint: ignore-end[tests_mirror_real_usage]

/// A command judge runs the whole member through onejudge's `split` provider:
/// the agent side stays on the real harness path, the supervisor is the command.
///
/// This is the contract's `judge: {command: [...]}` alternative.
#[test]
fn a_command_judge_supervises_through_the_split_provider() {
    let workspace = Workspace::new();
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            // Single-quoted, because this is a path and one of the three
            // platforms spells paths with backslashes: in a double-quoted YAML
            // scalar `C:\Users` is an unknown escape and the graph will not
            // parse. A single-quoted scalar takes backslashes literally.
            "    judge:\n      command: ['{provider}']\n",
            "    mode: bypass\n",
        ),
        fake = fake_harness(),
        provider = fake_provider(),
    ));
    // A base carrying evals and an assessment reaches every operation the
    // protocol has, so the whole command-provider surface is driven rather than
    // just the completion decision.
    workspace.write(
        "base.yaml",
        concat!(
            "provider:\n  kind: oneharness\n",
            "agent:\n  instructions: |\n    Standing bar: verify before you claim done.\n",
            "user:\n  done_when: \"the task is complete\"\n  max_turns: 4\n",
            "evals:\n",
            "  - criterion: \"the change is well-scoped\"\n    kind: numeric\n    scale: [1, 5]\n",
            "assessment: \"Name the follow-up work this run left out of scope.\"\n",
        ),
    );
    let run = workspace.run_task("fake:complete-now: judged by a command");
    run.expect_code(0);
    assert_eq!(
        run.of_kind("member-settled")[0]["payload"]["completed"],
        serde_json::json!(true)
    );

    // And the other half of the same supervisor: a member that never reaches its
    // bar is asked for another turn until the cap, then settles incomplete.
    let incomplete = workspace.run_task("fake:should-fail: judged by a command");
    incomplete.expect_code(1);
    assert_eq!(
        incomplete.of_kind("member-settled")[0]["payload"]["completed"],
        serde_json::json!(false)
    );
}

/// A run with no task at all refuses before it launches anything, rather than
/// spending a turn on a member with nothing to do.
#[test]
fn a_member_with_no_task_refuses_before_it_launches() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(2);
    assert!(run.stderr.contains("no task"), "{}", run.stderr);
    assert!(
        run.stdout.is_empty(),
        "a refusal must not read as an event stream"
    );
}

/// A persona that does not satisfy the delta contract refuses the run, naming
/// what is wrong — before a paid turn is spent on it.
///
/// Ported from `test_dispatch_unknown_persona_raises` and
/// `test_dispatch_rejects_unsafe_persona_names`.
#[test]
fn an_unusable_persona_refuses_the_run_before_anything_starts() {
    let workspace = Workspace::new();
    workspace.write(
        "roles/empty.yaml",
        "agent:\n  instructions: '  '\nuser:\n  persona: ''\n",
    );
    let cases = [
        ("./roles/empty.yaml", "agent.instructions is required"),
        ("./roles/nowhere.yaml", "cannot read"),
        ("./roles/typo.yaml", "unknown field"),
    ];
    workspace.write("roles/typo.yaml", "agent:\n  instrucions: typo\n");
    for (reference, expected) in cases {
        workspace.graph(
            &two_party_graph(&fake_harness(), "")
                .replace("persona: engineer", &format!("persona: {reference}")),
        );
        let run = workspace.run_task("fake:complete-now: never gets here");
        run.expect_code(2);
        assert!(run.stderr.contains(expected), "{reference}: {}", run.stderr);
    }
}

/// A graph whose base config merges to something incomplete refuses, naming the
/// field the base never supplied.
#[test]
fn an_incomplete_base_config_refuses_the_run() {
    let workspace = Workspace::new();
    workspace.write("base.yaml", "provider:\n  kind: oneharness\n");
    let run = workspace.run_task("fake:complete-now: incomplete base");
    run.expect_code(2);
    assert!(run.stderr.contains("user.done_when"), "{}", run.stderr);
}

/// `--set` reaches the member's own field, and a path naming nothing refuses
/// rather than running a member on a setting nobody applied.
///
/// Ported from `test_dispatch_provider_override`.
#[test]
fn a_set_override_reaches_the_member_and_a_bad_one_refuses() {
    let workspace = Workspace::new();
    // `persona` is optional and deliberately absent from this real graph. Two
    // flags also prove the established left-to-right, last-one-wins ordering.
    workspace.graph(&two_party_graph(&fake_harness(), "").replace("    persona: engineer\n", ""));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: overridden",
        "--dir",
        &workspace.dir().display().to_string(),
        "--set",
        "members.worker.mode=read-only",
        "--set",
        "members.worker.persona=reviewer",
        "--set",
        "members.worker.persona=engineer",
    ]);
    run.expect_code(0);
    assert_eq!(
        labels(&run.of_kind("member-started")[0])["persona"],
        "engineer"
    );

    let refused = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: never",
        "--dir",
        &workspace.dir().display().to_string(),
        "--set",
        "members.ghost.mode=read-only",
    ]);
    refused.expect_code(2);
    assert!(refused.stderr.contains("no ghost"), "{}", refused.stderr);

    // The same schema probe supplies an absent optional number's type.
    let numeric = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: retuned",
        "--dir",
        &workspace.dir().display().to_string(),
        "--set",
        "members.worker.max_turns=3",
        "--set",
        "members.worker.persona=engineer",
        "--set",
        "members.worker.agent.stream=false",
    ]);
    numeric.expect_code(0);

    workspace.graph(&single_sided_graph(&fake_harness()));
    let list = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: list override",
        "--dir",
        &workspace.dir().display().to_string(),
        "--set",
        "members.reporter.deps=[]",
    ]);
    list.expect_code(0);

    workspace.graph(&two_party_graph(&fake_harness(), "").replace("    persona: engineer\n", ""));

    for assignment in [
        "members.worker.max_turns=soon",
        "members.worker.ghost=x",
        "members.worker.schedule.every=3",
    ] {
        let refused = workspace.run(&[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: never",
            "--dir",
            &workspace.dir().display().to_string(),
            "--set",
            assignment,
        ]);
        refused.expect_code(2);
        assert!(
            refused
                .stderr
                .contains(assignment.split('=').next().unwrap()),
            "{assignment}: {}",
            refused.stderr
        );
    }

    workspace.graph(&two_party_graph(&fake_harness(), "").replace("    mode: bypass\n", ""));
    let missing_required = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: never",
        "--dir",
        &workspace.dir().display().to_string(),
        "--set",
        "members.worker.mode=bypass",
    ]);
    missing_required.expect_code(2);
    assert!(
        missing_required.stderr.contains("members.worker.mode"),
        "{}",
        missing_required.stderr
    );

    // A `--set` value arrives as text, but the field it lands on has a type in
    // the graph document. Overriding a number with a quoted string would change
    // the document's shape rather than its value, and the schema would then
    // refuse a graph the caller thought they had only retuned. So the graph here
    // spells the two typed fields out, which the default one leaves unset.
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    max_turns: 4\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n      stream: true\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n",
        ),
        fake = fake_harness(),
    ));
    let numeric = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: retuned",
        "--dir",
        &workspace.dir().display().to_string(),
        "--set",
        "members.worker.max_turns=3",
    ]);
    numeric.expect_code(0);

    for (assignment, expected) in [
        ("members.worker.max_turns=soon", "not a number"),
        ("members.worker.agent.stream=maybe", "not a boolean"),
    ] {
        let mistyped = workspace.run(&[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: never",
            "--dir",
            &workspace.dir().display().to_string(),
            "--set",
            assignment,
        ]);
        mistyped.expect_code(2);
        assert!(
            mistyped.stderr.contains(expected),
            "{assignment}: {}",
            mistyped.stderr
        );
    }
}

/// `--task-file` is the other way in, and naming both is a refusal rather than a
/// silent preference.
#[test]
fn the_task_arrives_by_file_and_naming_both_ways_refuses() {
    let workspace = Workspace::new();
    let task = workspace.write("task.md", "fake:complete-now: from a file\n");
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task-file",
        &task.display().to_string(),
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    let both = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "one",
        "--task-file",
        &task.display().to_string(),
    ]);
    both.expect_code(2);
    assert!(both.stderr.contains("exactly one"), "{}", both.stderr);
}

/// `deps` decides the order: a dependant's first event follows its dependency's
/// settle, in the one merged stream.
#[test]
fn a_dependant_member_starts_only_after_its_dependency_settles() {
    let workspace = Workspace::new();
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n",
            "  build:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  report:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    deps: [build]\n",
        ),
        fake = fake_harness(),
    ));
    let run = workspace.run_task("fake:complete-now: ordered");
    run.expect_code(0);

    let events = run.events();
    let position = |member: &str, kind: &str| {
        events
            .iter()
            .position(|event| {
                event["kind"] == kind && labels(event).get("member").is_some_and(|m| m == member)
            })
            .unwrap_or_else(|| panic!("no {kind} for {member}"))
    };
    assert!(
        position("build", "member-settled") < position("report", "member-started"),
        "the dependant started before its dependency settled"
    );
}

/// A run under a base config the member cannot even read refuses with the path,
/// not a parse trace.
#[test]
fn an_unreadable_ref_refuses_with_the_path_it_could_not_read() {
    let workspace = Workspace::new();
    workspace.graph(&two_party_graph(&fake_harness(), "").replace("./base.yaml", "./nowhere.yaml"));
    let run = workspace.run_task("fake:complete-now: unreadable");
    run.expect_code(2);
    assert!(run.stderr.contains("nowhere.yaml"), "{}", run.stderr);
}

/// A single-sided member whose `oneharness` binary is not there dies as
/// `unstartable`, with the stream saying so — rather than the run reporting a
/// settled graph.
///
/// This is the member kind that still *is* a child process, so it is where a
/// binary that cannot be spawned is still reachable. What it proves about the
/// payload is the half the conversion changed: a member that never started has
/// no exit code, no disposition, and no standard error, so it says why through
/// `cause` and `detail` instead of reporting a process's facts as null.
#[test]
fn a_member_that_cannot_start_is_a_death_the_stream_names() {
    let workspace = Workspace::new();
    workspace.graph(&single_sided_graph(&fake_harness()));
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: unstartable",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[(
            "ONEAGENTGRAPH_ONEHARNESS_BIN",
            "oneharness-that-is-not-installed",
        )],
    );
    run.expect_code(1);
    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{died:?}");
    let payload = &died[0]["payload"];
    assert_eq!(payload["rule"], serde_json::json!("unstartable"));
    assert_eq!(payload["cause"], serde_json::json!("spawn"));
    assert!(
        payload["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("oneharness-that-is-not-installed"),
        "{died:?}"
    );
    for absent in ["exit_code", "disposition", "stderr_tail"] {
        assert!(
            payload.get(absent).is_none(),
            "a member that never started reported {absent}: {payload}"
        );
    }
}

/// The base and the persona are recorded content-addressed, so a replay is
/// checked against what was read rather than against a path or a URL.
#[test]
fn every_config_a_run_read_is_recorded_content_addressed() {
    let workspace = Workspace::new();
    workspace
        .run_task("fake:complete-now: recorded")
        .expect_code(0);

    let record = workspace.record();
    let refs = record["refs"].as_array().expect("refs").clone();
    let origins: Vec<&str> = refs.iter().filter_map(|r| r["origin"].as_str()).collect();
    for expected in [
        "./graph.yaml",
        "./base.yaml",
        "./oneharness.toml",
        "./oneharness.judge.toml",
    ] {
        assert!(
            origins.contains(&expected),
            "{origins:?} is missing {expected}"
        );
    }
    for reference in &refs {
        let digest = reference["sha256"].as_str().expect("a digest");
        assert_eq!(digest.len(), 64, "{reference}");
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()), "{reference}");
    }
    // The two sides name the same file, and content addressing is what makes
    // that one entry rather than two that merely look alike.
    assert_eq!(
        refs.iter()
            .filter(|r| r["origin"] == "./oneharness.toml")
            .count(),
        1
    );
}

/// A `kind: oneharness` member is single-sided: one agent, no judge, and the
/// same stream shape.
#[test]
fn a_single_sided_member_runs_one_agent_with_no_judge() {
    let workspace = Workspace::new();
    workspace.write("oneharness.toml", CHAIN);
    workspace.graph(&format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        fake = fake_harness(),
    ));
    let run = workspace.run_task("fake:complete-now: single sided");
    run.expect_code(0);
    assert_eq!(
        labels(&run.of_kind("member-settled")[0])["member"],
        "reporter"
    );
    assert!(
        !run.of_kind("turn-activity").is_empty(),
        "{:?}",
        run.kinds()
    );
}
