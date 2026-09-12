//! Journeys for a member's own say over whether it holds the run open —
//! `background`, from graph schema version 8 — against the compiled binary and
//! the real oneharness process.
//!
//! Every journey here observes the run's *liveness*: whether it stays open once
//! the member holding it open has settled, whether a clock fires again in that
//! time, and whether a turn in flight at that moment is drained or a chain
//! started past it. None reads an effective config back, because the field is
//! a claim about what the run does, and that is the only thing that proves it.

// llmlint: ignore-file[e2e_not_mocked] these journeys use the repository's sole
// sanctioned fake at oneharness's ONEHARNESS_BIN_<ID> paid-provider seam. The
// compiled oneagentgraph and real oneharness CLI/process boundary remain real.

use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

use oneagentgraph::liveness::BACKGROUND_ENV;

use crate::support::{fake_harness, graph_with, until, Workspace, FAKE_HARNESS_KEY};

/// The graph every liveness journey below drives: a `worker` that holds the run
/// open until `release` exists, beside a `ticker` on a one-second cadence whose
/// first turn is deferred one second — so its clock starts with the graph rather
/// than after the wave the worker takes the whole of.
///
/// `version` is the schema the document declares, `ticker` is what its member
/// says beyond that — nothing, or a `background:` line — and `extra` is any
/// further member the journey needs. Numbers are substituted into the skeleton
/// rather than passed through [`graph_with`], which writes every value as a
/// string, and the paths go through it for the reason it gives.
fn paced_graph(fake: &str, release: &str, version: u32, ticker: &str, extra: &str) -> String {
    let skeleton = format!(
        concat!(
            "version: {version}\nname: paced\n",
            "env: {{}}\n",
            "members:\n",
            "  worker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  ticker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    schedule: {{every: 1, start_after: 1}}\n{ticker}{extra}",
        ),
        version = version,
        ticker = ticker,
        extra = extra,
    );
    graph_with(
        &skeleton,
        &[
            (FAKE_HARNESS_KEY, fake.to_string()),
            (
                "members.worker.task",
                format!("fake:complete-now hold this run open. fake:hold={release}"),
            ),
            (
                "members.ticker.task",
                "fake:complete-now report progress.".to_string(),
            ),
        ],
    )
}

/// `paced_graph` under the current schema, with its ticker saying nothing.
fn default_graph(fake: &str, release: &str) -> String {
    paced_graph(fake, release, 8, "", "")
}

/// `paced_graph` under the current schema, with its ticker declared foreground.
fn foreground_ticker_graph(fake: &str, release: &str) -> String {
    paced_graph(fake, release, 8, "    background: false\n", "")
}

/// Start the run of this workspace's graph, in the run's own environment.
fn spawn(workspace: &Workspace, args: &[&str], env: &[(&str, &str)]) -> Child {
    let dir = workspace.dir().display().to_string();
    let mut argv = vec!["run", "./graph.yaml", "--dir", &dir];
    argv.extend_from_slice(args);
    workspace.spawn_with(&argv, env)
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

/// How many envelopes of `kind` a member has published onto `stream`.
fn count(stream: &str, kind: &str, member: &str) -> usize {
    stream
        .lines()
        .filter(|line| {
            line.contains(&format!("\"kind\":\"{kind}\""))
                && line.contains(&format!("\"member\":\"{member}\""))
        })
        .count()
}

/// The stream after `member`'s first `member-settled`, which is the moment it
/// finished — everything a journey asserts did *not* happen past that moment is
/// read from here. Nothing, while it has not settled.
fn after_settle_of<'a>(stream: &'a str, member: &str) -> Option<&'a str> {
    let mut offset = 0;
    for line in stream.split_inclusive('\n') {
        offset += line.len();
        if line.contains("\"kind\":\"member-settled\"")
            && line.contains(&format!("\"member\":\"{member}\""))
        {
            return Some(&stream[offset..]);
        }
    }
    None
}

/// How many envelopes of `kind` `member` published after `finished` settled —
/// none, while it has not.
fn count_after(stream: &str, finished: &str, kind: &str, member: &str) -> usize {
    after_settle_of(stream, finished).map_or(0, |after| count(after, kind, member))
}

/// Wait `for_` and then say whether the run is still going.
fn still_running_after(child: &mut Child, for_: Duration) -> bool {
    std::thread::sleep(for_);
    child.try_wait().expect("waitable").is_none()
}

/// The run's exit code once it ends — and a run that does not end inside the
/// suite's patience is a failed assertion rather than a hung suite.
fn exit_of(mut child: Child) -> (Option<i32>, String) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("waitable").is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        child.try_wait().expect("waitable").is_some(),
        "the run never settled"
    );
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

/// Release the worker and give the run the two ticks it needs to notice.
fn release(path: &Path) {
    std::fs::write(path, "release").expect("release the worker");
}

/// A scheduled member that states nothing is a pacemaker: it fires while
/// foreground work is live and never again once the last foreground member has
/// finished, and the run exits 0.
///
/// The default, driven through a document that names the field nowhere. The
/// second half is the one that matters — a clock that went on firing into a
/// finished run is the thirty-nine accidental settles this field exists to end,
/// read the other way round.
#[test]
fn a_scheduled_member_at_its_default_fires_while_foreground_work_is_live_and_never_after() {
    let workspace = Workspace::new();
    let release_at = workspace.at("release");
    let document = default_graph(&fake_harness(), &release_at.display().to_string());
    assert!(!document.contains("background"), "{document}");
    workspace.graph(&document);
    let child = spawn(&workspace, &[], &[]);
    until(
        "the ticker to fire twice while the worker holds the run",
        || count(&stream(&workspace), "cron-fired", "ticker") >= 2,
    );

    release(&release_at);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");
    let stream = stream(&workspace);
    assert_eq!(
        count_after(&stream, "worker", "cron-fired", "ticker"),
        0,
        "a background clock fired after the last foreground member finished: {stream}"
    );
    assert_eq!(workspace.record()["members"]["ticker"], "settled");
    assert_eq!(workspace.record()["members"]["worker"], "settled");
}

/// A scheduled member declared `background: false` keeps the run open past the
/// settlement of every unscheduled member, fires again in that time, and holds
/// it until it is cancelled — at which point the run exits 0.
///
/// The declaration working rather than a bug: a scheduled foreground member
/// never finishes on its own, so the run lives as long as it does.
#[test]
fn a_scheduled_member_declared_foreground_holds_the_run_open_until_it_is_cancelled() {
    let workspace = Workspace::new();
    let release_at = workspace.at("release");
    workspace.graph(&foreground_ticker_graph(
        &fake_harness(),
        &release_at.display().to_string(),
    ));
    let mut child = spawn(&workspace, &[], &[]);
    until("the worker to start", || {
        count(&stream(&workspace), "member-started", "worker") == 1
    });

    release(&release_at);
    until("the worker to settle", || {
        count(&stream(&workspace), "member-settled", "worker") == 1
    });
    until(
        "the ticker to fire after the last unscheduled member settled",
        || count_after(&stream(&workspace), "worker", "cron-fired", "ticker") >= 2,
    );
    assert!(
        still_running_after(&mut child, Duration::from_millis(500)),
        "the run settled with a foreground schedule still on its clock"
    );

    workspace
        .run(&["cancel", &run_id(&workspace), "ticker"])
        .expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(workspace.record()["members"]["ticker"], "settled");
}

/// `--set members.<id>.background=…` beats both the environment and the
/// document, in either direction.
///
/// Two runs, each observed through the run's liveness rather than its config: a
/// ticker the environment backgrounds and the flag un-backgrounds holds the run
/// open, and one the document holds open and the flag backgrounds does not.
#[test]
fn a_set_override_beats_the_environment_and_the_document() {
    // The environment says background, the flag says foreground: the run stays
    // open until the ticker is cancelled.
    let workspace = Workspace::new();
    let release_at = workspace.at("release");
    workspace.graph(&default_graph(
        &fake_harness(),
        &release_at.display().to_string(),
    ));
    let mut child = spawn(
        &workspace,
        &["--set", "members.ticker.background=false"],
        &[(BACKGROUND_ENV, "ticker")],
    );
    until("the worker to start", || {
        count(&stream(&workspace), "member-started", "worker") == 1
    });
    release(&release_at);
    until("the ticker to fire after the worker settled", || {
        count_after(&stream(&workspace), "worker", "cron-fired", "ticker") >= 1
    });
    assert!(
        still_running_after(&mut child, Duration::from_millis(500)),
        "the flag lost to the environment: the run settled over a member --set held open"
    );
    workspace
        .run(&["cancel", &run_id(&workspace), "ticker"])
        .expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");

    // The document says foreground, the flag says background: the run settles
    // as soon as the worker does, with no further firing.
    let workspace = Workspace::new();
    let release_at = workspace.at("release");
    workspace.graph(&foreground_ticker_graph(
        &fake_harness(),
        &release_at.display().to_string(),
    ));
    let child = spawn(
        &workspace,
        &["--set", "members.ticker.background=true"],
        &[],
    );
    until("the ticker to fire while the worker holds the run", || {
        count(&stream(&workspace), "cron-fired", "ticker") >= 1
    });
    release(&release_at);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");
    let stream = stream(&workspace);
    assert_eq!(
        count_after(&stream, "worker", "cron-fired", "ticker"),
        0,
        "the flag lost to the document: a member --set backgrounded fired on: {stream}"
    );
}

/// `ONEAGENTGRAPH_BACKGROUND` marks the members it names background, beating
/// their documents — and only those: read the way an operator types it, with
/// each entry trimmed and empty ones ignored, so `" ticker, pacer,"` marks
/// `ticker` and `pacer` and nothing else.
///
/// Three scheduled members the document holds the run open with. The variable
/// names two; the third is what keeps the run alive once the worker settles, and
/// cancelling it is what proves the other two were backgrounded — a run with
/// either still foreground would not settle.
#[test]
fn the_environment_backgrounds_the_members_it_names_and_only_those() {
    let workspace = Workspace::new();
    let release_at = workspace.at("release");
    let document = paced_graph(
        &fake_harness(),
        &release_at.display().to_string(),
        8,
        "    background: false\n",
        concat!(
            "  pacer:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    task: fake:complete-now pace.\n",
            "    schedule: {every: 1, start_after: 1}\n    background: false\n",
            "  keeper:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    task: fake:complete-now keep.\n",
            "    schedule: {every: 3600, start_after: 3600}\n    background: false\n",
        ),
    );
    workspace.graph(&document);
    let mut child = spawn(&workspace, &[], &[(BACKGROUND_ENV, " ticker, pacer,")]);
    until("the worker to start", || {
        count(&stream(&workspace), "member-started", "worker") == 1
    });
    release(&release_at);
    until("the worker to settle", || {
        count(&stream(&workspace), "member-settled", "worker") == 1
    });
    // The keeper the variable did not name is foreground as its document says,
    // so the run stays open — and the backgrounded clocks keep firing while it
    // does, because background is about what holds the run open, not about
    // who may take a turn while it is.
    until(
        "both backgrounded clocks to fire while the keeper holds the run",
        || {
            let stream = stream(&workspace);
            count_after(&stream, "worker", "cron-fired", "ticker") >= 1
                && count_after(&stream, "worker", "cron-fired", "pacer") >= 1
        },
    );
    assert!(
        still_running_after(&mut child, Duration::from_millis(500)),
        "the variable backgrounded a member it never named"
    );

    workspace
        .run(&["cancel", &run_id(&workspace), "keeper"])
        .expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(
        code,
        Some(0),
        "a member the variable named still held the run open: {stderr}"
    );
    for member in ["ticker", "pacer", "worker"] {
        assert_eq!(workspace.record()["members"][member], "settled");
    }
}

/// The variable is read from the environment `run` is started in, never from
/// the graph's own `env:` block — that block is exported *to* members.
#[test]
fn the_environment_is_the_runs_own_and_never_the_graphs_env_block() {
    assert!(
        std::env::var_os(BACKGROUND_ENV).is_none(),
        "this journey needs {BACKGROUND_ENV} unset around the suite"
    );
    let workspace = Workspace::new();
    let release_at = workspace.at("release");
    let document = graph_with(
        &foreground_ticker_graph(&fake_harness(), &release_at.display().to_string()),
        &[(&format!("env.{BACKGROUND_ENV}"), "ticker")],
    );
    assert!(
        document.contains(&format!("{BACKGROUND_ENV}: ticker")),
        "{document}"
    );
    workspace.graph(&document);
    let mut child = spawn(&workspace, &[], &[]);
    until("the worker to start", || {
        count(&stream(&workspace), "member-started", "worker") == 1
    });
    release(&release_at);
    until("the ticker to fire after the worker settled", || {
        count_after(&stream(&workspace), "worker", "cron-fired", "ticker") >= 1
    });
    assert!(
        still_running_after(&mut child, Duration::from_millis(500)),
        "the graph's own env block backgrounded a member the run's environment never named"
    );
    workspace
        .run(&["cancel", &run_id(&workspace), "ticker"])
        .expect_code(0);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");
}

/// The variable naming a member the graph does not have is exit 2 before any
/// member starts, naming the variable and the name; naming a member under a
/// document older than version 8 is refused exactly as the field is — and so is
/// a `--set` naming it there, and a document that spells it itself.
#[test]
fn background_is_refused_by_name_and_version_however_it_is_named() {
    let workspace = Workspace::new();
    let release_at = workspace.at("release");
    workspace.graph(&default_graph(
        &fake_harness(),
        &release_at.display().to_string(),
    ));
    let dir = workspace.dir().display().to_string();
    let run_args = ["run", "./graph.yaml", "--dir", &dir];

    let refused = workspace.run_with(&run_args, &[(BACKGROUND_ENV, "ticker, ghost")]);
    refused.expect_code(2);
    assert!(
        refused.stderr.contains(BACKGROUND_ENV),
        "{}",
        refused.stderr
    );
    assert!(refused.stderr.contains("\"ghost\""), "{}", refused.stderr);
    assert!(
        refused.stdout.trim().is_empty(),
        "a refused run published something: {}",
        refused.stdout
    );
    assert!(
        std::fs::read_dir(workspace.state())
            .expect("state")
            .next()
            .is_none(),
        "a refused run recorded itself"
    );

    // The same graph a version behind: the variable, the flag, and the
    // document itself are all refused for the field and the version it needs.
    let older = paced_graph(
        &fake_harness(),
        &release_at.display().to_string(),
        7,
        "",
        "",
    );
    workspace.graph(&older);
    let expected = "uses `background`, which requires graph schema version 8";
    let refused = workspace.run_with(&run_args, &[(BACKGROUND_ENV, "ticker")]);
    refused.expect_code(2);
    assert!(refused.stderr.contains(expected), "{}", refused.stderr);
    assert!(refused.stderr.contains("\"ticker\""), "{}", refused.stderr);
    let refused = workspace.run(&[
        "run",
        "./graph.yaml",
        "--dir",
        &dir,
        "--set",
        "members.ticker.background=true",
    ]);
    refused.expect_code(2);
    assert!(refused.stderr.contains(expected), "{}", refused.stderr);
    workspace.graph(&paced_graph(
        &fake_harness(),
        &release_at.display().to_string(),
        7,
        "    background: true\n",
        "",
    ));
    for verb in [&run_args[..], &["validate", "./graph.yaml"][..]] {
        let refused = workspace.run(verb);
        refused.expect_code(2);
        assert!(refused.stderr.contains(expected), "{}", refused.stderr);
    }
    assert!(
        std::fs::read_dir(workspace.state())
            .expect("state")
            .next()
            .is_none(),
        "a refused run recorded itself"
    );

    // And a version 8 document that omits it everywhere is accepted.
    workspace.graph(&default_graph(
        &fake_harness(),
        &release_at.display().to_string(),
    ));
    workspace.run(&["validate", "./graph.yaml"]).expect_code(0);
}

/// A deferred first turn that nothing holds the run open for is refused by
/// `validate` and by `run`, naming the member and `background: false` as an
/// answer; one foreground member — scheduled, or taking a turn in the initial
/// waves — is what makes the same graph accepted; and a version 7 document in
/// the same shape is refused as it always was.
#[test]
fn a_deferred_turn_nothing_holds_open_is_refused_and_one_foreground_member_holds_it() {
    let workspace = Workspace::new();
    let all_scheduled = |version: u32, ticker: &str, extra: &str| {
        graph_with(
            &format!(
                concat!(
                    "version: {}\nname: unheld\nenv: {{}}\nmembers:\n",
                    "  ticker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
                    "    task: fake:complete-now tick.\n",
                    "    schedule: {{every: 3600}}\n{}",
                    "  pacer:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
                    "    task: fake:complete-now pace.\n",
                    "    schedule: {{every: 3600, start_after: 0}}\n{}",
                ),
                version, ticker, extra
            ),
            &[(FAKE_HARNESS_KEY, fake_harness())],
        )
    };
    let dir = workspace.dir().display().to_string();

    workspace.graph(&all_scheduled(8, "", ""));
    for verb in [
        &["validate", "./graph.yaml"][..],
        &["run", "./graph.yaml", "--dir", &dir][..],
    ] {
        let refused = workspace.run(verb);
        refused.expect_code(2);
        assert!(refused.stderr.contains("(ticker)"), "{}", refused.stderr);
        assert!(!refused.stderr.contains("pacer)"), "{}", refused.stderr);
        assert!(
            refused.stderr.contains("background: false"),
            "{}",
            refused.stderr
        );
        assert!(refused.stdout.trim().is_empty(), "{}", refused.stdout);
    }
    assert!(
        std::fs::read_dir(workspace.state())
            .expect("state")
            .next()
            .is_none(),
        "a refused run recorded itself"
    );

    // A version 7 document in the same shape is refused as it always was — and
    // in the words it always was, which never mention the field it predates.
    workspace.graph(&all_scheduled(7, "", ""));
    let refused = workspace.run(&["validate", "./graph.yaml"]);
    refused.expect_code(2);
    assert!(refused.stderr.contains("(ticker)"), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("scheduled or descends from one"),
        "{}",
        refused.stderr
    );
    assert!(!refused.stderr.contains("background"), "{}", refused.stderr);

    // One foreground scheduled member holds it open — the deferred one itself,
    // or its sibling — and so does one unscheduled member taking a turn in the
    // initial waves; an unscheduled member declared background, or one whose
    // dependency defers its own first turn, does not.
    for (ticker, extra, accepted) in [
        ("    background: false\n", "", true),
        ("", "    background: false\n", true),
        (
            "",
            "  worker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n    task: fake:complete-now work.\n",
            true,
        ),
        (
            "",
            "  worker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n    task: fake:complete-now work.\n    background: true\n",
            false,
        ),
        (
            "",
            "  worker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n    task: fake:complete-now work.\n    deps: [ticker]\n",
            false,
        ),
    ] {
        workspace.graph(&all_scheduled(8, ticker, extra));
        let answer = workspace.run(&["validate", "./graph.yaml"]);
        answer.expect_code(if accepted { 0 } else { 2 });
    }
}

/// A member declared `background: true` whose turn is in flight when the last
/// foreground member finishes is run to its end and recorded; from that moment
/// no background member starts a new turn — not from a clock firing, and not
/// from the chain that turn was part of — and the run exits 0.
///
/// The graph is a pacemaker's chain: `ticker` fires, `report` runs, `publish`
/// runs after it. The first firing's chain runs whole while `keeper` holds the
/// run, which is what proves `publish` *does* follow `report` when the run is
/// live. The second firing's `report` is held in flight while `keeper` settles,
/// and then released: it settles and is recorded, `publish` never starts again,
/// a `trigger` written in the meantime fires nothing, and the run ends.
#[test]
fn a_background_turn_in_flight_at_the_last_foreground_finish_is_drained_and_nothing_follows_it() {
    let workspace = Workspace::new();
    let keeper_release = workspace.at("keeper-release");
    let report_release = workspace.at("report-release");
    workspace.graph(&chain_graph(
        &fake_harness(),
        &keeper_release.display().to_string(),
        &format!(
            "fake:complete-now report. fake:hold={}",
            report_release.display()
        ),
    ));
    let mut child = spawn(&workspace, &[], &[]);
    until("the first firing to reach the held report", || {
        let stream = stream(&workspace);
        count(&stream, "cron-fired", "ticker") == 1
            && count(&stream, "member-started", "report") == 1
    });
    assert!(
        still_running_after(&mut child, Duration::from_millis(200)),
        "the run settled over a held report"
    );
    release(&report_release);
    until("the first chain to run whole", || {
        count(&stream(&workspace), "member-settled", "publish") == 1
    });
    std::fs::remove_file(&report_release).expect("re-arm the hold");

    let id = run_id(&workspace);
    workspace.run(&["trigger", &id, "ticker"]).expect_code(0);
    until("the second firing to reach the held report", || {
        let stream = stream(&workspace);
        count(&stream, "cron-fired", "ticker") == 2
            && count(&stream, "member-started", "report") == 2
    });

    // The last foreground member finishes with the report's turn in flight.
    release(&keeper_release);
    until("the keeper to settle", || {
        count(&stream(&workspace), "member-settled", "keeper") == 1
    });
    // A firing asked for now is one the run no longer takes.
    workspace.run(&["trigger", &id, "ticker"]).expect_code(0);
    assert!(
        still_running_after(&mut child, Duration::from_secs(2)),
        "the run settled with a background turn still in flight"
    );
    assert_eq!(
        count(&stream(&workspace), "member-settled", "report"),
        1,
        "the held turn was ended rather than drained"
    );

    release(&report_release);
    let (code, stderr) = exit_of(child);
    assert_eq!(code, Some(0), "{stderr}");
    let stream = stream(&workspace);
    assert_eq!(
        count(&stream, "member-settled", "report"),
        2,
        "the drained turn was not run to its end: {stream}"
    );
    let after = after_settle_of(&stream, "keeper").expect("the keeper settled");
    assert_eq!(
        count(after, "member-started", "publish"),
        0,
        "a chain started a background member after the last foreground one finished: {stream}"
    );
    assert_eq!(
        count(after, "cron-fired", "ticker"),
        0,
        "a background clock fired after the last foreground member finished: {stream}"
    );
    for member in ["keeper", "ticker", "report", "publish"] {
        assert_eq!(
            workspace.record()["members"][member],
            "settled",
            "{member}: {}",
            workspace.record()
        );
    }
}

/// A background turn drained that way and ending in failure is recorded with
/// that failure as its outcome, and the run exits 1 rather than 0.
///
/// The report never reaches its bar, so the one turn it takes — the drained one
/// — is the only failure in the run, and the exit code is its.
#[test]
fn a_drained_background_turn_that_fails_fails_the_run() {
    let workspace = Workspace::new();
    let keeper_release = workspace.at("keeper-release");
    let report_release = workspace.at("report-release");
    workspace.graph(&chain_graph(
        &fake_harness(),
        &keeper_release.display().to_string(),
        &format!(
            "fake:should-fail report. fake:hold={}",
            report_release.display()
        ),
    ));
    let mut child = spawn(&workspace, &[], &[]);
    until("the firing to reach the held report", || {
        count(&stream(&workspace), "member-started", "report") == 1
    });
    release(&keeper_release);
    until("the keeper to settle", || {
        count(&stream(&workspace), "member-settled", "keeper") == 1
    });
    assert!(
        still_running_after(&mut child, Duration::from_secs(2)),
        "the run settled with a background turn still in flight"
    );

    release(&report_release);
    let (code, stderr) = exit_of(child);
    assert_eq!(
        code,
        Some(1),
        "a drained turn's failure did not fail the run: {stderr}"
    );
    let record = workspace.record();
    assert_eq!(record["members"]["report"], "incomplete", "{record}");
    assert_eq!(record["members"]["keeper"], "settled", "{record}");
    assert_eq!(record["exit_code"], 1, "{record}");
    let stream = stream(&workspace);
    assert_eq!(
        count_after(&stream, "keeper", "member-started", "publish"),
        0,
        "{stream}"
    );
}

/// A pacemaker's chain beside a two-party `keeper` holding the run open:
/// `ticker` defers its first turn a second and fires on demand after that,
/// `report` — two-party, `background: true`, given `report_task` — follows each
/// firing, and `publish` follows a successful `report`. Both chain members are
/// background; under version 8 an unscheduled member is foreground by default,
/// and a chain of foreground members would be a run that never settles while its
/// pacemaker keeps it fed.
fn chain_graph(fake: &str, keeper_release: &str, report_task: &str) -> String {
    graph_with(
        concat!(
            "version: 8\nname: chained\n",
            "env: {}\n",
            "members:\n",
            "  keeper:\n    kind: onejudge\n    base_config: ./base.yaml\n",
            "    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n",
            "  ticker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    task: fake:complete-now tick.\n",
            "    schedule: {every: 3600, start_after: 1}\n",
            "  report:\n    kind: onejudge\n    base_config: ./base.yaml\n",
            "    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n    background: true\n    deps: [ticker]\n",
            "  publish:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    task: fake:complete-now publish.\n",
            "    background: true\n    deps: [report]\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake.to_string()),
            (
                "members.keeper.task",
                format!("fake:complete-now keep this run open. fake:hold={keeper_release}"),
            ),
            ("members.report.task", report_task.to_string()),
        ],
    )
}

/// A graph whose every member is background takes each non-deferred first turn
/// once and settles 0 — a scheduled member firing at t=0, an unscheduled member
/// declared background behind it, and nothing else.
#[test]
fn a_graph_of_only_background_members_takes_its_initial_turns_and_settles() {
    let workspace = Workspace::new();
    workspace.graph(&graph_with(
        concat!(
            "version: 8\nname: all-background\n",
            "env: {}\n",
            "members:\n",
            "  ticker:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    task: fake:complete-now tick.\n",
            "    schedule: {every: 1, start_after: 0}\n",
            "  report:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    task: fake:complete-now report.\n",
            "    background: true\n    deps: [ticker]\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
    ));
    let started = Instant::now();
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "an all-background graph did not settle once its initial turns were done"
    );
    assert!(run.of_kind("cron-fired").is_empty(), "{}", run.stdout);
    for member in ["ticker", "report"] {
        assert_eq!(
            run.of_kind("member-started")
                .iter()
                .filter(|event| {
                    crate::support::labels(event)
                        .get("member")
                        .map(String::as_str)
                        == Some(member)
                })
                .count(),
            1,
            "{member} took other than one turn: {}",
            run.stdout
        );
        assert_eq!(workspace.record()["members"][member], "settled");
    }
}
