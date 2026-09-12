//! A two-party member's own job — its worktree and its pace — against the
//! compiled binary and real onejudge turns.
//!
//! From graph schema version 9 a `kind: onejudge` member carries the `dir` and
//! `schedule` a `kind: oneharness` member has carried since version 3. The
//! journeys here are what those two fields *do* on a conversation: `dir` is the
//! directory the agent side's harness is started in and the one its skills are
//! discovered from, and `schedule` paces one conversation's turns rather than
//! starting a conversation per firing. Everything asserted is read off the run's
//! own stream or off what the harness double recorded about where it ran —
//! never off a launch value.

// llmlint: ignore-file[e2e_not_mocked] the same declaration its sibling journey
// files carry, and for the same reason — see tests/e2e/support.rs: the paid
// harness process is the one sanctioned double, replaced at oneharness's own
// `ONEHARNESS_BIN_<ID>` seam. Real oneagentgraph resolves the member's directory,
// the real onejudge engine it links opens the conversation there, and real
// oneharness starts the double in it.

use std::path::{Path, PathBuf};

use crate::support::{
    fake_harness, graph_with, Run, Workspace, FAKE_HARNESS_KEY, UNREACHABLE_CHAIN,
    UNREACHABLE_HARNESS_KEY,
};

/// Where Claude Code discovers a project skill from its working directory —
/// the layout `fake:project-skill` reads, so a skill planted here under one
/// directory is distinguishable from the same name planted under another.
fn plant_skill(root: &Path, name: &str, body: &str) {
    let dir = root.join(".claude").join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("the skill directory");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: a skill\n---\n{body}\n"),
    )
    .expect("the skill");
}

/// The default two-party graph under the current schema, with `dir` on the
/// worker when `dir` names one.
fn graph(workspace: &Workspace, dir: Option<&str>) -> String {
    let mut values = vec![(FAKE_HARNESS_KEY.to_string(), fake_harness())];
    // A second identity the decoy config below names, made unreachable on every
    // host — so a member that *did* pick the decoy up dies rather than spending a
    // real turn where Codex happens to be installed.
    values.push((
        UNREACHABLE_HARNESS_KEY.to_string(),
        workspace.unreachable_harness(),
    ));
    if let Some(dir) = dir {
        values.push(("members.worker.dir".to_string(), dir.to_string()));
    }
    graph_with(
        concat!(
            "version: 9\nname: node-scope\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n",
        ),
        &values,
    )
}

/// Every directory a harness recorded through `fake:record-cwd`, canonical so a
/// host whose temporary directory is a symlink compares equal to what the graph
/// named.
fn recorded_dirs(path: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|err| {
            panic!(
                "no harness recorded a directory in {}: {err}",
                path.display()
            )
        })
        .lines()
        .map(|line| canonical(Path::new(line.trim())))
        .collect()
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|err| panic!("{} cannot be resolved: {err}", path.display()))
}

/// The words the agent side spoke on the stream, one string per turn.
fn agent_messages(run: &Run) -> Vec<String> {
    run.of_kind("turn-message")
        .into_iter()
        .filter(|event| event["payload"]["role"] == "assistant")
        .filter_map(|event| event["payload"]["text"].as_str().map(str::to_string))
        .collect()
}

/// A member's stamped agent config with the one per-run value in it — the
/// scratch stamp, which names the run that wrote it — replaced by a placeholder,
/// so two runs' configs can be compared for everything that is not the run.
fn without_run_stamp(config: &str) -> String {
    config
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("ONEAGENTGRAPH_SCRATCH_DIR") {
                "ONEAGENTGRAPH_SCRATCH_DIR = <this run>".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drive the default two-party graph with the worker's `dir` set to
/// `declared`, which resolves to `resolved`, and assert all three consequences
/// the contract gives that field.
///
/// The three, each read off the real conversation: the agent side's harness was
/// started in `resolved` (the double records where it was started); the
/// conversation's skills are discovered from `resolved` rather than from the
/// run's `--dir` (a skill of one name is planted under each, with different
/// bodies, and the transcript shows which one the agent side read); and the
/// agent side still runs under its stamped config whatever `dir` says — proven
/// the sharp way, with a decoy `oneharness.toml` under `resolved` naming an
/// unreachable chain, which a side discovering its config from its directory
/// would have died on.
fn worker_works_in(workspace: &Workspace, declared: &str, resolved: &Path) -> Run {
    std::fs::create_dir_all(resolved).expect("the member's own directory");
    plant_skill(&workspace.dir(), "scope", "the skill under the run's --dir");
    plant_skill(resolved, "scope", "the skill under the member's own dir");
    std::fs::write(resolved.join("oneharness.toml"), UNREACHABLE_CHAIN).expect("a decoy config");
    let cwd = workspace.at("worker.cwd");
    workspace.graph(&graph(workspace, Some(declared)));

    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &format!(
            "fake:project-skill=scope report what your project skill says. fake:record-cwd={}",
            cwd.display()
        ),
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    // Where the harnesses were started: the member's own directory, and never
    // the run's — every side, because onejudge gives the judge the same worktree
    // as evidence context.
    let dirs = recorded_dirs(&cwd);
    assert!(
        dirs.contains(&canonical(resolved)),
        "the agent side's harness was not started in the member's own directory: {dirs:?}"
    );
    assert!(
        !dirs.contains(&canonical(&workspace.dir())),
        "a side of a member with its own `dir` was started in the run's --dir: {dirs:?}"
    );

    // Which directory's skill reached the conversation, off the transcript.
    let spoken = agent_messages(&run);
    assert!(
        spoken
            .iter()
            .any(|text| text.contains("the skill under the member's own dir")),
        "the skill under the member's own `dir` never reached the conversation: {spoken:?}"
    );
    assert!(
        !spoken
            .iter()
            .any(|text| text.contains("the skill under the run's --dir")),
        "the skill under the run's --dir reached a conversation told to work elsewhere: \
         {spoken:?}"
    );

    // And the member settled on the doubled chain, which is the stamped config's
    // — a side that had discovered the decoy under `dir` would have died on an
    // unreachable harness instead.
    assert_eq!(workspace.record()["members"]["worker"], "settled");
    assert!(
        run.of_kind("member-died").is_empty(),
        "the agent side died, which is what discovering the decoy config under `dir` looks \
         like: {}",
        run.stdout
    );
    run
}

/// The stamped agent config a member runs under is the same one whether or not
/// it names a `dir`, everything but the run's own scratch stamp byte-identical.
fn assert_same_stamped_config(with_dir: &Workspace, without_dir: &Workspace) {
    let pinned = without_run_stamp(&with_dir.member_file("worker", "oneharness.toml"));
    let baseline = without_run_stamp(&without_dir.member_file("worker", "oneharness.toml"));
    assert_eq!(
        pinned, baseline,
        "a member with a `dir` loads a different agent config from one without"
    );
    assert!(
        pinned.contains("claude-code"),
        "the stamped config is not the graph's own chain: {pinned}"
    );
}

/// The baseline every `dir` journey compares its stamped config against: the
/// same graph, the same task, and no `dir`.
fn baseline_without_dir() -> Workspace {
    let workspace = Workspace::new();
    workspace.graph(&graph(&workspace, None));
    workspace
        .run(&[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now report in.",
            "--dir",
            &workspace.dir().display().to_string(),
        ])
        .expect_code(0);
    workspace
}

/// A relative `dir` on a two-party member is the conversation's worktree, one
/// level inside the run's `--dir`: the agent side's harness starts there, the
/// conversation's skills come from there, and the agent side still runs under
/// its stamped config.
#[test]
fn a_two_party_members_relative_dir_is_the_conversations_worktree() {
    let workspace = Workspace::new();
    let resolved = workspace.dir().join("api");
    worker_works_in(&workspace, "./api", &resolved);
    assert_same_stamped_config(&workspace, &baseline_without_dir());
}

/// An absolute `dir` on a two-party member is used as written — a worktree that
/// is not below the run's `--dir` at all — with the same three consequences.
#[test]
fn a_two_party_members_absolute_dir_is_the_conversations_worktree() {
    let workspace = Workspace::new();
    let elsewhere = tempfile::tempdir().expect("a worktree elsewhere");
    let declared = elsewhere.path().display().to_string();
    worker_works_in(&workspace, &declared, elsewhere.path());
    assert_same_stamped_config(&workspace, &baseline_without_dir());
}

/// An empty `dir` on a two-party member is refused by `validate` and by `run`,
/// before any member is built, exactly as a single-sided member's is.
#[test]
fn a_two_party_members_empty_dir_is_refused_by_both_verbs() {
    let workspace = Workspace::new();
    workspace.graph(&graph(&workspace, Some("")));
    for args in [
        vec!["validate", "./graph.yaml"],
        vec!["run", "./graph.yaml", "--task", "fake:complete-now"],
    ] {
        let refused = workspace.run(&args);
        refused.expect_code(2);
        assert!(
            refused.stderr.contains("names no directory") && refused.stderr.contains("worker"),
            "{}: {}",
            args.join(" "),
            refused.stderr
        );
        assert!(
            refused.stdout.trim().is_empty(),
            "a refusal must publish no event: {}",
            refused.stdout
        );
    }
}

// ---------------------------------------------------------------------------
// A paced conversation: `schedule` on a two-party member.
// ---------------------------------------------------------------------------

use std::process::Child;
use std::time::{Duration, Instant};

use oneagentgraph::control::{self, Accepted, Addressee, Note, NoteDelivery};
use oneagentgraph::run::{MemberName, RunId};
use serde_json::Value;

use crate::support::{as_env, bounds, fake_provider, oneharness_bin, until, BASE};

/// The judge side every paced journey runs: the command double, which continues
/// a `fake:should-fail` conversation turn after turn and stops at `max_turns`.
///
/// Real onejudge drives every turn; the double only answers the supervisor's
/// question the way a `should-fail` task asks it to.
const FAKE_JUDGE: &str = "judge.command.0";

/// A graph whose `worker` is a paced conversation on the command judge, plus
/// whatever `extra` members the journey needs.
///
/// `schedule` and `settings` are substituted into the skeleton rather than
/// passed through [`graph_with`], which writes every value as a string: a
/// schedule's seconds are a number, and a `background:` is a boolean, and either
/// quoted into the document would be refused by the schema before any journey
/// reached what it tests. `max_turns` is where a paced conversation ends on its
/// own.
fn paced_graph(schedule: &str, settings: &str, max_turns: u32, extra: &str) -> String {
    let skeleton = format!(
        concat!(
            "version: 9\nname: paced\n",
            "env: {{}}\n",
            "members:\n",
            "  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      command: [placeholder]\n",
            "    mode: bypass\n",
            "    max_turns: {max_turns}\n",
            "    schedule: {schedule}\n{settings}{extra}",
        ),
        max_turns = max_turns,
        schedule = schedule,
        settings = settings,
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

/// A single-sided member beside the paced one, on the doubled harness, whose
/// task is `task`.
fn beside(name: &str, settings: &str, task: &str) -> String {
    let mut document = format!(
        concat!(
            "  {name}:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n{settings}",
        ),
        name = name,
        settings = settings,
    );
    // Through the serializer, for the reason `graph_with` gives.
    let quoted = serde_norway::to_string(&serde_norway::Value::String(task.to_string()))
        .expect("a task serializes");
    document.push_str(&format!("    task: {}", quoted.trim_start_matches("---\n")));
    if !document.ends_with('\n') {
        document.push('\n');
    }
    document
}

/// The default onejudge base without a `done_when`, with `system` appended to
/// the agent's standing system prompt.
///
/// Without the criterion, a paced conversation's settle is the loop's own — the
/// turn cap, or the supervisor's completion — rather than a re-judged criterion
/// the command double scores a `should-fail` task unmet on. The system prompt is
/// the one text every **agent** turn carries: a task steers only the turn it
/// opens, so a sentinel that has to fire on a later turn goes here.
fn base_without_done_when(system: &str) -> String {
    let mut document: serde_norway::Value =
        serde_norway::from_str(BASE).expect("the base is a YAML document");
    let mapping = document.as_mapping_mut().expect("a mapping");
    let key = serde_norway::Value::String("system_prompt".to_string());
    let standing = mapping
        .get(&key)
        .and_then(serde_norway::Value::as_str)
        .unwrap_or_default()
        .to_string();
    mapping.insert(
        key,
        serde_norway::Value::String(format!("{standing}\n{system}\n")),
    );
    let user = mapping
        .get_mut("user")
        .expect("the base has a user block")
        .as_mapping_mut()
        .expect("user is a mapping");
    user.remove("done_when");
    serde_norway::to_string(&document).expect("the base serializes")
}

/// The task every held conversation below runs on: one the command judge
/// continues turn after turn.
const KEEP_GOING: &str = "fake:should-fail keep going";

/// Start the run of this workspace's graph on `task`, holding `env` beyond the
/// default.
fn spawn(workspace: &Workspace, task: &str, env: &[(&str, &str)]) -> Child {
    let dir = workspace.dir().display().to_string();
    let mut pairs = vec![(
        "XDG_STATE_HOME",
        workspace.session_store().display().to_string(),
    )];
    pairs.extend(env.iter().map(|(key, value)| (*key, (*value).to_string())));
    let borrowed: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    workspace.spawn_with(
        &["run", "./graph.yaml", "--task", task, "--dir", &dir],
        &borrowed,
    )
}

/// This run's merged event stream, as it stands, parsed.
fn stream(workspace: &Workspace) -> Vec<Value> {
    std::fs::read_dir(workspace.state())
        .into_iter()
        .flatten()
        .flatten()
        .next()
        .map(|entry| entry.path().join("events.jsonl"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Every envelope of `kind` that `member` published, in stream order.
fn of<'a>(events: &'a [Value], kind: &str, member: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["kind"] == kind && event["labels"]["member"] == member)
        .collect()
}

/// The turns one side of `member`'s conversation opened, in stream order.
fn turns_of<'a>(events: &'a [Value], member: &str, role: &str, kind: &str) -> Vec<&'a Value> {
    of(events, kind, member)
        .into_iter()
        .filter(|event| event["payload"]["role"] == role)
        .collect()
}

/// An envelope's `ts` as milliseconds since the epoch.
///
/// The stamp is RFC 3339 at millisecond precision in UTC — `tests/e2e/dispatch.rs`
/// pins that shape — so it is read here rather than by a date library the suite
/// does not otherwise carry. Days from the civil date by the usual arithmetic.
fn millis(event: &Value) -> i64 {
    let ts = event["ts"].as_str().expect("every envelope carries ts");
    let (date, rest) = ts.split_at(10);
    let mut ymd = date
        .split('-')
        .map(|part| part.parse::<i64>().expect("a date part"));
    let (year, month, day) = (
        ymd.next().expect("year"),
        ymd.next().expect("month"),
        ymd.next().expect("day"),
    );
    let clock = rest.trim_start_matches('T').trim_end_matches('Z');
    let mut hms = clock.split(':');
    let hour: i64 = hms.next().expect("hour").parse().expect("hour");
    let minute: i64 = hms.next().expect("minute").parse().expect("minute");
    let second: f64 = hms.next().expect("second").parse().expect("second");
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + (second * 1000.0).round() as i64
}

/// The gap, in milliseconds, from `earlier` to `later`.
fn gap(earlier: &Value, later: &Value) -> i64 {
    millis(later) - millis(earlier)
}

/// Whether `earlier` precedes `later` on the stream.
fn precedes(events: &[Value], earlier: &Value, later: &Value) -> bool {
    let at = |wanted: &Value| events.iter().position(|event| event == wanted);
    match (at(earlier), at(later)) {
        (Some(before), Some(after)) => before < after,
        _ => false,
    }
}

/// Wait until `worker`'s conversation is in a hold: its supervisor has closed
/// `turn` with a next instruction, and the next worker turn has not opened.
///
/// The hold begins the instant the supervisor's `turn-completed` is on the
/// stream — the sink publishes it and then waits — so its presence, with no
/// later worker `turn-started`, is the conversation held.
fn until_held(workspace: &Workspace, turn: usize) {
    until(
        &format!("the worker to be held after its supervisor's turn {turn}"),
        || {
            let events = stream(workspace);
            turns_of(&events, "worker", "user", "turn-completed").len() >= turn
                && turns_of(&events, "worker", "assistant", "turn-started").len() == turn
        },
    );
}

/// The run's exit code once it ends, and its stderr; a run that outlives the
/// suite's patience is ended here rather than left holding its members open.
fn exit_of(mut child: Child) -> (Option<i32>, String) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while child.try_wait().expect("waitable").is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if child.try_wait().expect("waitable").is_none() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("the run never settled");
    }
    let output = child.wait_with_output().expect("the run finishes");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// The id of the one run this workspace has recorded.
fn run_id(workspace: &Workspace) -> String {
    workspace.record()["run_id"]
        .as_str()
        .expect("run id")
        .to_string()
}

/// A paced conversation is **one** conversation whose turns are held `every`
/// seconds apart: every agent turn after the first opens no sooner than `every`
/// after the judge's preceding turn completed, a `cron-fired` sits between them,
/// the member settles exactly once, and its turn numbers run on.
///
/// The turn numbers are the proof it is one conversation rather than one per
/// firing: a fresh conversation per firing would open every turn as turn 1.
#[test]
fn a_paced_conversation_holds_every_between_its_turns_and_settles_once() {
    const EVERY: i64 = 2;
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    workspace.graph(&paced_graph(
        &format!("{{every: {EVERY}, start_after: 0}}"),
        "    background: false\n",
        3,
        "",
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:should-fail keep going",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    // The conversation ran to its cap without completing, which the command
    // judge answers for a `should-fail` task; the pacing is what is under test.
    run.expect_code(1);
    let events = run.events();

    let agent_turns = turns_of(&events, "worker", "assistant", "turn-started");
    let judge_turns = turns_of(&events, "worker", "user", "turn-completed");
    assert_eq!(agent_turns.len(), 3, "{}", run.stdout);
    assert_eq!(
        agent_turns
            .iter()
            .map(|turn| turn["payload"]["turn"].as_u64().expect("a turn number"))
            .collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the turns of one conversation run on rather than restarting"
    );
    let fired = of(&events, "cron-fired", "worker");
    assert_eq!(fired.len(), 2, "one firing per hold: {}", run.stdout);
    for (index, opened) in agent_turns.iter().enumerate().skip(1) {
        let closed = judge_turns[index - 1];
        let held = gap(closed, opened);
        assert!(
            held >= EVERY * 1000,
            "agent turn {} opened {held}ms after the judge closed turn {}, sooner than every: {}",
            index + 1,
            index,
            run.stdout
        );
        assert!(
            precedes(&events, closed, fired[index - 1])
                && precedes(&events, fired[index - 1], opened),
            "no cron-fired between the judge's close and the next agent turn: {}",
            run.stdout
        );
    }
    assert_eq!(of(&events, "member-settled", "worker").len(), 1);
    assert_eq!(of(&events, "member-started", "worker").len(), 1);
    assert_eq!(workspace.record()["members"]["worker"], "incomplete");
}

/// A hold is skipped when the judge's closing turn carried no next instruction:
/// a paced conversation the judge completes settles at once, not `every` later.
#[test]
fn a_judges_completion_settles_a_paced_conversation_without_a_hold() {
    const EVERY: u64 = 120;
    let workspace = Workspace::new();
    workspace.graph(&paced_graph(
        &format!("{{every: {EVERY}, start_after: 0}}"),
        "    background: false\n",
        4,
        "",
    ));
    let launched = Instant::now();
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now finish at once",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);
    let took = launched.elapsed();
    assert!(
        took < Duration::from_secs(EVERY / 2),
        "a completed conversation waited out a hold before settling: {took:?}"
    );
    let events = run.events();
    assert_eq!(of(&events, "member-settled", "worker").len(), 1);
    assert!(
        of(&events, "cron-fired", "worker").is_empty(),
        "{}",
        run.stdout
    );
    assert_eq!(workspace.record()["members"]["worker"], "settled");
}

/// `start_after` defers a paced conversation's **first** turn — the member
/// comes up with its wave and says so, no turn opens before the delay, and the
/// conversation opens when it elapses — and `start_after: 0` opens it in the
/// wave.
#[test]
fn start_after_defers_a_paced_conversations_first_turn_and_zero_opens_it_in_the_wave() {
    const START_AFTER: i64 = 2;
    let workspace = Workspace::new();
    workspace.graph(&paced_graph(
        &format!("{{every: 1, start_after: {START_AFTER}}}"),
        "    background: false\n",
        2,
        "",
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now finish at once",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);
    let events = run.events();
    let started = of(&events, "member-started", "worker");
    assert_eq!(
        started.len(),
        2,
        "one for coming up, one for the turn: {}",
        run.stdout
    );
    assert_eq!(
        started[0]["payload"]["start_after"],
        Value::from(START_AFTER)
    );
    let first_turn = turns_of(&events, "worker", "assistant", "turn-started")[0];
    assert!(
        gap(started[0], first_turn) >= START_AFTER * 1000,
        "the first turn opened before the delay elapsed: {}",
        run.stdout
    );
    let fired = of(&events, "cron-fired", "worker");
    assert_eq!(fired.len(), 1, "{}", run.stdout);
    assert!(precedes(&events, fired[0], first_turn));

    // `start_after: 0`: in the wave, with no delay announced and no firing.
    let at_once = Workspace::new();
    at_once.graph(&paced_graph(
        "{every: 1, start_after: 0}",
        "    background: false\n",
        2,
        "",
    ));
    let run = at_once.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now finish at once",
        "--dir",
        &at_once.dir().display().to_string(),
    ]);
    run.expect_code(0);
    let events = run.events();
    let started = of(&events, "member-started", "worker");
    assert_eq!(started.len(), 1, "{}", run.stdout);
    assert!(
        started[0]["payload"].get("start_after").is_none(),
        "{}",
        run.stdout
    );
    assert!(
        of(&events, "cron-fired", "worker").is_empty(),
        "{}",
        run.stdout
    );
}

/// The graph every hold-control journey below drives: a paced worker whose
/// conversation the command judge continues, held long enough that a turn which
/// opens "at once" is unmistakable, and whose run is otherwise over at its cap.
fn held_worker(every: u64, max_turns: u32, settings: &str) -> String {
    paced_graph(
        &format!("{{every: {every}, start_after: 0, resettable: true}}"),
        settings,
        max_turns,
        "",
    )
}

/// A `trigger` during a hold opens the next turn at once.
#[test]
fn a_trigger_during_a_hold_opens_the_next_turn_at_once() {
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    workspace.graph(&held_worker(60, 2, "    background: false\n"));
    let child = spawn(&workspace, KEEP_GOING, &[]);
    until_held(&workspace, 1);
    let id = run_id(&workspace);

    let triggered = Instant::now();
    workspace.run(&["trigger", &id, "worker"]).expect_code(0);
    until("the next turn to open on the trigger", || {
        turns_of(&stream(&workspace), "worker", "assistant", "turn-started").len() >= 2
    });
    assert!(
        triggered.elapsed() < Duration::from_secs(30),
        "the trigger did not end the hold"
    );
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");
    let events = stream(&workspace);
    assert_eq!(of(&events, "cron-fired", "worker").len(), 1);
    assert_eq!(of(&events, "member-settled", "worker").len(), 1);
}

/// A note offered during a hold ends it: the next turn opens at once and the
/// note is delivered into it as `queued`, the offer answered inside its own
/// deadline and the delivery on the stream.
#[test]
fn a_note_offered_during_a_hold_opens_the_next_turn_and_is_delivered_queued() {
    let workspace = Workspace::new();
    let prompts = workspace.at("agent-prompts");
    workspace.write(
        "base.yaml",
        &base_without_done_when(&format!("fake:record-prompt={}", prompts.display())),
    );
    workspace.graph(&held_worker(60, 2, "    background: false\n"));
    let child = spawn(&workspace, KEEP_GOING, &[]);
    until_held(&workspace, 1);
    let id = RunId::parse(&run_id(&workspace)).expect("a run id");
    let member = MemberName::parse("worker").expect("a member name");

    let text = "sent into a hold: take it on the next turn";
    let note = Note::new(Addressee::Worker, text).expect("a note with text in it");
    let offered = Instant::now();
    let delivery = control::note(&workspace.state(), &id, &member, &note, &oneharness_bin())
        .expect("the run and its member are addressable");
    let answered = offered.elapsed();
    assert_eq!(
        delivery,
        NoteDelivery::Accepted(Accepted::Queued),
        "a note offered into a hold was not held for the next turn to open"
    );
    assert!(
        answered < Duration::from_secs(30),
        "the offer was answered only after {answered:?}, past the submitter's deadline"
    );

    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");
    let events = stream(&workspace);
    let turns = turns_of(&events, "worker", "assistant", "turn-started");
    assert_eq!(turns.len(), 2, "the note did not open the next turn");
    assert_eq!(
        turns[1]["payload"]["origin"], "delivered",
        "the turn the note opened does not say it carries one"
    );
    let delivered = of(&events, "turn-interrupted", "worker");
    assert!(
        delivered
            .iter()
            .any(|event| event["payload"]["delivered"] == true),
        "the delivery was not published on the stream: {events:?}"
    );
    let handed = std::fs::read_to_string(&prompts).unwrap_or_default();
    assert!(
        handed.contains(text),
        "the note never reached the next worker turn: {handed}"
    );
}

/// A member `cancel` during a hold ends the conversation without another turn,
/// as a cancel ends it mid-turn; so does the whole run's `stop`.
#[test]
fn a_cancel_or_stop_during_a_hold_ends_the_conversation_without_another_turn() {
    for scope in [Some("worker"), None] {
        let workspace = Workspace::new();
        workspace.write("base.yaml", &base_without_done_when(""));
        workspace.graph(&held_worker(60, 3, "    background: false\n"));
        let child = spawn(&workspace, KEEP_GOING, &[]);
        until_held(&workspace, 1);
        let id = run_id(&workspace);

        let mut args = vec!["cancel", id.as_str()];
        args.extend(scope);
        workspace.run(&args).expect_code(0);
        let (code, stderr) = exit_of(child);
        assert_eq!(code, Some(1), "{scope:?}: {stderr}");
        let events = stream(&workspace);
        assert_eq!(
            turns_of(&events, "worker", "assistant", "turn-started").len(),
            1,
            "{scope:?}: a turn opened after the cancel"
        );
        let died = of(&events, "member-died", "worker");
        assert_eq!(died.len(), 1, "{scope:?}: {events:?}");
        assert_eq!(died[0]["payload"]["cause"], "cancelled", "{scope:?}");
        assert!(of(&events, "member-settled", "worker").is_empty());
        assert_eq!(
            workspace.record()["members"]["worker"],
            "died (provider-failure)"
        );
    }
}

/// For a paced member left at its default — background — the run's quiescence
/// ends the conversation at its hold: no further turn, a settled outcome of its
/// own, and the run exits 0.
#[test]
fn quiescence_ends_a_background_paced_conversation_at_its_hold_as_settled() {
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    let release = workspace.at("anchor-release");
    workspace.graph(&paced_graph(
        "{every: 60, start_after: 0}",
        "",
        4,
        &beside(
            "anchor",
            "",
            &format!(
                "fake:complete-now hold the run open. fake:hold={}",
                release.display()
            ),
        ),
    ));
    let child = spawn(&workspace, KEEP_GOING, &[]);
    until_held(&workspace, 1);

    std::fs::write(&release, "release").expect("release the anchor");
    let released = Instant::now();
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        released.elapsed() < Duration::from_secs(30),
        "the run waited out the hold before settling"
    );
    let events = stream(&workspace);
    assert_eq!(
        turns_of(&events, "worker", "assistant", "turn-started").len(),
        1
    );
    assert_eq!(of(&events, "member-settled", "worker").len(), 1);
    assert!(of(&events, "member-died", "worker").is_empty());
    assert_eq!(workspace.record()["members"]["worker"], "settled");
    assert_eq!(workspace.record()["members"]["anchor"], "settled");
    let report = workspace.member_file("worker", "report.json");
    assert!(
        report.contains("only background members left"),
        "the report does not say the run's quiescence ended it: {report}"
    );
}

/// A `reset-timer` on a resettable schedule restarts the hold in progress and
/// publishes `cron-reset`; one during the initial delay restores the whole delay
/// rather than promoting the member to its cadence.
#[test]
fn a_reset_restarts_a_hold_and_restores_an_initial_delay() {
    const EVERY: i64 = 4;
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    workspace.graph(&held_worker(EVERY as u64, 2, "    background: false\n"));
    let child = spawn(&workspace, KEEP_GOING, &[]);
    until_held(&workspace, 1);
    let id = run_id(&workspace);
    // Into the hold, so a reset that merely let the original count run would
    // be told apart from one that restarted it.
    std::thread::sleep(Duration::from_millis(1500));
    workspace
        .run(&["reset-timer", &id, "worker"])
        .expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");
    let events = stream(&workspace);
    let reset = of(&events, "cron-reset", "worker");
    assert_eq!(reset.len(), 1, "{events:?}");
    let second = turns_of(&events, "worker", "assistant", "turn-started")[1];
    assert!(
        gap(reset[0], second) >= EVERY * 1000,
        "the next turn opened {}ms after the reset, sooner than the restarted hold",
        gap(reset[0], second)
    );

    // The initial delay, reset: the whole of `start_after` again.
    const START_AFTER: i64 = 4;
    let deferred = Workspace::new();
    deferred.graph(&paced_graph(
        &format!("{{every: 1, start_after: {START_AFTER}, resettable: true}}"),
        "    background: false\n",
        1,
        "",
    ));
    let child = spawn(&deferred, "fake:complete-now finish at once", &[]);
    until("the deferred member to come up", || {
        !of(&stream(&deferred), "member-started", "worker").is_empty()
    });
    std::thread::sleep(Duration::from_millis(1500));
    let id = run_id(&deferred);
    deferred.run(&["reset-timer", &id, "worker"]).expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");
    let events = stream(&deferred);
    let reset = of(&events, "cron-reset", "worker");
    assert_eq!(reset.len(), 1, "{events:?}");
    let first = turns_of(&events, "worker", "assistant", "turn-started")[0];
    assert!(
        gap(reset[0], first) >= START_AFTER * 1000,
        "the first turn opened {}ms after the reset, sooner than the restored delay",
        gap(reset[0], first)
    );
}

/// A `trigger` during the initial delay opens the first turn at once, and one
/// that arrives while a turn is live is honoured at the next hold rather than
/// lost.
#[test]
fn a_trigger_opens_a_deferred_first_turn_at_once_and_one_mid_turn_is_honoured_at_the_next_hold() {
    let workspace = Workspace::new();
    workspace.graph(&paced_graph(
        "{every: 1, start_after: 60}",
        "    background: false\n",
        1,
        "",
    ));
    let child = spawn(&workspace, "fake:complete-now finish at once", &[]);
    until("the deferred member to come up", || {
        !of(&stream(&workspace), "member-started", "worker").is_empty()
    });
    let id = run_id(&workspace);
    let triggered = Instant::now();
    workspace.run(&["trigger", &id, "worker"]).expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        triggered.elapsed() < Duration::from_secs(30),
        "the trigger did not open the deferred first turn"
    );
    assert_eq!(
        turns_of(&stream(&workspace), "worker", "assistant", "turn-started").len(),
        1
    );

    // Mid-turn: the agent's first turn is held open on a file, the trigger is
    // left while it is live, and the next turn opens as soon as the judge has
    // closed — no `every` in between.
    let live = Workspace::new();
    live.write("base.yaml", &base_without_done_when(""));
    live.graph(&held_worker(60, 2, "    background: false\n"));
    let hold = live.at("turn-hold");
    let dir = live.dir().display().to_string();
    let child = live.spawn_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            &format!("fake:should-fail keep going fake:hold={}", hold.display()),
            "--dir",
            &dir,
        ],
        &[],
    );
    until("the first turn to be live", || {
        !turns_of(&stream(&live), "worker", "assistant", "turn-started").is_empty()
    });
    let id = run_id(&live);
    live.run(&["trigger", &id, "worker"]).expect_code(0);
    std::fs::write(&hold, "go").expect("release the turn");
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");
    let events = stream(&live);
    let closed = turns_of(&events, "worker", "user", "turn-completed")[0];
    let second = turns_of(&events, "worker", "assistant", "turn-started")[1];
    assert!(
        gap(closed, second) < 30_000,
        "a trigger left mid-turn was not honoured at the next hold: {}ms",
        gap(closed, second)
    );
    assert_eq!(of(&events, "cron-fired", "worker").len(), 1);
}

/// A hold is never condemned as a stall, and the watchdog is neither weakened
/// nor silenced for it: with the stall bound below `every`, a paced member
/// survives its hold and takes its next turn, its heartbeats continue through
/// the hold at their ordinary cadence — and a member of the same graph that
/// genuinely stalls is still condemned.
#[test]
fn a_hold_is_not_a_stall_and_a_real_stall_beside_it_is_still_condemned() {
    const EVERY: u64 = 8;
    const STALL: &str = "3";
    const HEARTBEAT: &str = "8";
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    workspace.graph(&paced_graph(
        &format!("{{every: {EVERY}, start_after: 0}}"),
        "    background: false\n",
        2,
        &beside("stuck", "", "fake:hang and never answer"),
    ));
    let env = bounds(HEARTBEAT, STALL);
    let child = spawn(&workspace, KEEP_GOING, &as_env(&env));
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(1), "{stderr}");
    let events = stream(&workspace);

    // The paced member: two turns, no death.
    assert_eq!(
        turns_of(&events, "worker", "assistant", "turn-started").len(),
        2,
        "the paced member did not take its next turn: {events:?}"
    );
    assert!(
        of(&events, "member-died", "worker").is_empty(),
        "the paced member was condemned for its hold: {events:?}"
    );
    // Its heartbeats through the hold: the ones between the judge's close and
    // the firing that ended it, at the cadence the heartbeat bound sets —
    // published every quarter of it — with a tolerance of twice that for
    // scheduling and delivery jitter.
    let closed = turns_of(&events, "worker", "user", "turn-completed")[0];
    let fired = of(&events, "cron-fired", "worker")[0];
    let beats: Vec<&Value> = of(&events, "member-heartbeat", "worker")
        .into_iter()
        .filter(|beat| precedes(&events, closed, beat) && precedes(&events, beat, fired))
        .collect();
    let cadence: i64 = HEARTBEAT.parse::<i64>().expect("seconds") * 1000 / 4;
    assert!(
        beats.len() >= 3,
        "only {} heartbeats during a hold of {EVERY}s: {events:?}",
        beats.len()
    );
    for pair in beats.windows(2) {
        let between = gap(pair[0], pair[1]);
        assert!(
            between <= 2 * cadence,
            "a {between}ms gap between heartbeats during the hold, past twice the cadence"
        );
    }
    // And the stuck member, under the same bounds: condemned by the activity
    // rule, which is the protection covering nothing but the scheduled wait.
    let died = of(&events, "member-died", "stuck");
    assert_eq!(died.len(), 1, "{events:?}");
    assert_eq!(died[0]["payload"]["rule"], "activity");
}

/// A paced member declared `background: false` keeps the run open through its
/// holds until its conversation settles, when every other member has finished
/// — and when it settles the schedule starts no second conversation.
#[test]
fn a_foreground_paced_conversation_holds_the_run_open_until_it_settles_and_is_not_restarted() {
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    // Deferred, so it is on the run's clock rather than in the wave — the case
    // where nothing but the declaration keeps the run open for it.
    workspace.graph(&paced_graph(
        "{every: 1, start_after: 1}",
        "    background: false\n",
        3,
        &beside("quick", "", "fake:complete-now report in."),
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:should-fail keep going",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(1);
    let events = run.events();
    let quick_settled = of(&events, "member-settled", "quick")[0];
    let turns = turns_of(&events, "worker", "assistant", "turn-started");
    assert_eq!(turns.len(), 3, "{}", run.stdout);
    assert!(
        precedes(&events, quick_settled, turns[0]),
        "the run's other member had not finished before the paced conversation opened"
    );
    assert_eq!(of(&events, "member-settled", "worker").len(), 1);
    assert_eq!(
        of(&events, "member-started", "worker").len(),
        2,
        "coming up, and its one conversation — never a second: {}",
        run.stdout
    );
    assert_eq!(
        of(&events, "cron-fired", "worker").len(),
        3,
        "{}",
        run.stdout
    );
    let settled = events.last().expect("graph-settled");
    assert_eq!(settled["kind"], "graph-settled");
    assert_eq!(workspace.record()["members"]["worker"], "incomplete");
    assert_eq!(workspace.record()["members"]["quick"], "settled");
}

/// `interrupt` addressed to a paced member during a hold answers exit 3: no
/// controllable turn is in flight.
#[test]
fn an_interrupt_during_a_hold_answers_exit_three() {
    let workspace = Workspace::new();
    workspace.write("base.yaml", &base_without_done_when(""));
    workspace.graph(&held_worker(60, 2, "    background: false\n"));
    let child = spawn(&workspace, KEEP_GOING, &[]);
    until_held(&workspace, 1);
    let id = run_id(&workspace);
    let interrupted = workspace.run_with(
        &["interrupt", &id, "worker", "--input", "one more thing"],
        &[(
            "XDG_STATE_HOME",
            &workspace.session_store().display().to_string(),
        )],
    );
    interrupted.expect_code(3);
    assert_eq!(interrupted.of_kind("turn-interrupted").len(), 1);
    workspace.run(&["cancel", &id]).expect_code(0);
    let _ = exit_of(child);
}

/// The rules a two-party schedule is held to, and the version each field needs,
/// by `validate` and by `run` alike, before any member is built — and
/// `pre_turn` on a two-party member still refused by name.
#[test]
fn a_two_party_schedule_is_gated_and_ruled_by_both_verbs_before_any_member_is_built() {
    let workspace = Workspace::new();
    let refuses = |document: &str, expected: &[&str]| {
        workspace.graph(document);
        for args in [
            vec!["validate", "./graph.yaml"],
            vec!["run", "./graph.yaml", "--task", "fake:complete-now"],
        ] {
            let refused = workspace.run(&args);
            refused.expect_code(2);
            for text in expected {
                assert!(
                    refused.stderr.contains(text),
                    "{}: expected {text:?} in: {}",
                    args.join(" "),
                    refused.stderr
                );
            }
            assert!(
                refused.stdout.trim().is_empty(),
                "a refusal must publish no event: {}",
                refused.stdout
            );
        }
    };
    let ceiling = oneagentgraph::config::MAX_SCHEDULE_SECONDS;
    // Each field alone, under schemas that predate it — the first, the one
    // that gave the single-sided kind these fields, and the one just before.
    for older in [1, 3, 8] {
        for (field, own) in [
            ("dir", "    dir: ./api\n"),
            ("schedule", "    schedule: {every: 300}\n"),
        ] {
            let document = graph_with(
                &format!(
                    concat!(
                        "version: {older}\nname: gated\nenv: {{}}\n",
                        "members:\n  worker:\n    kind: onejudge\n",
                        "    base_config: ./base.yaml\n    persona: engineer\n",
                        "    agent:\n      oneharness_config: ./oneharness.toml\n",
                        "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
                        "    mode: bypass\n{own}",
                    ),
                    older = older,
                    own = own,
                ),
                &[(FAKE_HARNESS_KEY, fake_harness())],
            );
            refuses(
                &document,
                &[
                    &format!("onejudge `{field}`"),
                    "requires graph schema version 9",
                ],
            );
        }
    }
    refuses(
        &paced_graph("{every: 0}", "", 2, ""),
        &["worker", "never stops firing"],
    );
    refuses(
        &paced_graph(&format!("{{every: {}}}", ceiling + 1), "", 2, ""),
        &["worker", "`every`", "longer than any run"],
    );
    refuses(
        &paced_graph(
            &format!("{{every: 60, start_after: {}}}", ceiling + 1),
            "",
            2,
            "",
        ),
        &["worker", "`start_after`", "longer than any run"],
    );
    refuses(
        &paced_graph(
            "{every: 300}",
            "    pre_turn:\n      - {command: [queue-depth]}\n",
            2,
            "",
        ),
        &["pre_turn"],
    );
}
