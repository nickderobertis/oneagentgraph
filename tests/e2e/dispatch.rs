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

use std::collections::BTreeMap;

use crate::support::{
    fake_harness, fake_provider, graph_with, labels, oneharness_bin, single_sided_graph,
    two_party_graph, Workspace, BASE, CHAIN, FAKE_HARNESS_KEY, NO_ENV,
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

/// A single-sided member says the same thing the two-party one above does, and
/// differs only in the `engine`: it is driven in this process, through
/// oneharness's own library, over the config this crate resolved for it.
///
/// The pair with the journey above is the point. `member-started` carries a
/// `runner`, and a consumer branching on it has to be able to trust that the
/// fields beside it match — a member reported as a library call with an argv, or
/// as a process with none, is a stream that cannot be read. Both kinds are
/// `library` now, so what a consumer branches on to tell them apart is the
/// `engine`, which is the field the contract put there for exactly that.
#[test]
fn a_single_sided_member_reports_the_library_it_is_driven_through() {
    let workspace = Workspace::new();
    workspace.graph(&single_sided_graph(&fake_harness()));
    let run = workspace.run_task("fake:complete-now: one sided");
    run.expect_code(0);

    let started = run.of_kind("member-started");
    assert_eq!(started.len(), 1, "{started:?}");
    let payload = &started[0]["payload"];
    assert_eq!(payload["runner"], serde_json::json!("library"));
    assert_eq!(payload["engine"], serde_json::json!("oneharness"));
    assert!(
        payload["config"]
            .as_str()
            .is_some_and(|config| config.ends_with(".toml")),
        "the member named no resolved config: {payload}"
    );
    assert!(
        payload["worktree"]
            .as_str()
            .is_some_and(|worktree| !worktree.is_empty()),
        "the member named no directory to work in: {payload}"
    );
    // None of the three a child process has. This member is not one — no
    // `oneharness` process is spawned for its turn at all.
    for absent in ["program", "args", "cwd"] {
        assert!(payload.get(absent).is_none(), "{absent}: {payload}");
    }

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

/// A member is a **job**, not a copy of its graph: one carrying its own `task`
/// and `dir` is given those, and the member beside it — carrying neither — is
/// given the run's, unchanged.
///
/// Both halves in one run, because that is the shape the failure had. A graph
/// whose scheduled `check_in` member exists to write one status update received
/// the *orchestrator's* task verbatim — "drive run X to settlement" — since a
/// single-sided member had nowhere to hold prose of its own; it never reported,
/// and it acted on instructions addressed to somebody else. So what is asserted
/// is not that the field parses but that the two members were handed different
/// jobs, read back from the harness process each one actually started.
///
/// The directory here is **relative**, which is the form that has to be resolved
/// rather than passed through: `./api` means one level inside the run's own
/// `--dir`, and the child carrying it to oneharness is spawned in the member's
/// scratch, so a path left as written would land somewhere nobody named. The
/// absolute form is the journey below.
#[test]
fn a_single_sided_member_runs_its_own_job_beside_one_that_runs_the_graphs() {
    let workspace = Workspace::new();
    let elsewhere = workspace.dir().join("api");
    std::fs::create_dir_all(&elsewhere).expect("the member's own directory");
    let own = workspace.at("check-in.prompt");
    let graph_wide = workspace.at("worker.prompt");
    let own_cwd = workspace.at("check-in.cwd");
    let graph_wide_cwd = workspace.at("worker.cwd");

    workspace.graph(&graph_with(
        concat!(
            "version: 3\nname: node-scope\n",
            "env: {}\n",
            "members:\n",
            "  check_in:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    dir: ./api\n",
            "  worker:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            (
                "members.check_in.task",
                format!(
                    "fake:complete-now write one status update, and nothing else. \
                     fake:record-prompt={} fake:record-cwd={}",
                    own.display(),
                    own_cwd.display(),
                ),
            ),
        ],
    ));

    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &format!(
            "fake:complete-now drive this run to settlement. fake:record-prompt={} \
             fake:record-cwd={}",
            graph_wide.display(),
            graph_wide_cwd.display()
        ),
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    // What each member's harness was really given. The member with its own task
    // never sees the run's prose at all — which is the failure this closes: a
    // pacemaker told to drive the run is a second writer to a ledger somebody
    // else is already driving.
    // Read leniently rather than expected: a member never given its own task
    // never writes this file at all, and the absence is exactly the failure —
    // said as the assertion below rather than as a missing path.
    let carried = std::fs::read_to_string(&own).unwrap_or_default();
    assert!(
        carried.contains("write one status update"),
        "a member carrying its own task was not given it: {carried:?}"
    );
    assert!(
        !carried.contains("drive this run to settlement"),
        "a member with its own task was still handed the run's: {carried}"
    );
    let inherited = std::fs::read_to_string(&graph_wide).expect("the worker member's prompt");
    assert!(
        inherited.contains("drive this run to settlement"),
        "a member with no task of its own must still be given the run's: {inherited}"
    );

    // And where each one ran. `--cwd` is what this crate decides; the directory
    // the harness process reports is what the harness actually did with it, and
    // only the second is the guarantee an operator has — the member's persona
    // says it runs in a scratch that is not a checkout, and a harness that
    // started in the graph's directory instead would be a different member.
    let reported_own = std::fs::read_to_string(&own_cwd)
        .expect("the check-in member's own directory")
        .trim()
        .to_string();
    assert_eq!(
        std::path::Path::new(&reported_own)
            .canonicalize()
            .expect("the reported member directory"),
        elsewhere.canonicalize().expect("canonical"),
    );
    let reported_graph = std::fs::read_to_string(&graph_wide_cwd)
        .expect("the worker member's directory")
        .trim()
        .to_string();
    assert_eq!(
        std::path::Path::new(&reported_graph)
            .canonicalize()
            .expect("the reported graph directory"),
        workspace.dir().canonicalize().expect("canonical"),
        "a member with no directory of its own must still run in the graph's"
    );

    // The same answer in the stream, where a supervisor reads it: each member
    // names the directory it was told to work in.
    let started = run.of_kind("member-started");
    assert_eq!(started.len(), 2, "{started:?}");
    for event in &started {
        let told = event["payload"]["worktree"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let expected = if labels(event)["member"] == "check_in" {
            elsewhere.display().to_string()
        } else {
            workspace.dir().display().to_string()
        };
        assert_eq!(told, expected, "{}", event["payload"]);
    }
}

// llmlint: ignore-block[tests_mirror_real_usage] the two journeys below assert on
// a file the doubled harness wrote recording the directory it was started in and
// the argv it was given. That is the observation point *because* it is the
// subject: `--cwd` is a value this crate hands to oneharness, and the only place
// its arrival can be checked is the process that received it. A member whose
// harness started somewhere nobody named settles with a stream identical to a
// correct one, so the stream cannot hold this. Every assertion about behaviour —
// the exit code, the events, the published `worktree` — is still made through the
// CLI.
/// A two-party member's harness runs in the directory the graph was given with
/// `--dir`, and is still pinned to the oneharness config the graph named while it
/// does.
///
/// Both halves in one run, because the bug lived between them: the worktree was
/// also the only thing pinning the agent side, so moving it alone reverts the
/// member to whatever config sits above the operator's directory. The pin is
/// asserted against a decoy — a second, valid `oneharness.toml` in the directory
/// the member now works in, naming a different approval mode, which oneharness
/// spells on the harness's own argv. Take `MemberSpawn`'s `--config` arm out and
/// this half reddens while the directory half stays green.
#[test]
fn a_two_party_members_harness_runs_in_the_directory_the_graph_was_given() {
    let workspace = Workspace::new();
    let where_it_ran = workspace.at("worker.cwd");
    let argv = workspace.at("worker.argv");

    // The decoy: a config discovery would find first from the member's new
    // working directory. Valid, plausible, and wrong — it names the posture the
    // member was not given.
    workspace.write(
        "work/oneharness.toml",
        "run_mode = \"fallback\"\nharnesses = [\"claude-code\"]\nmode = \"bypass\"\n",
    );
    workspace.graph(
        &two_party_graph(&fake_harness(), NO_ENV).replace("mode: bypass", "mode: read-only"),
    );
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &format!(
            "fake:complete-now write the thing. fake:record-cwd={} fake:record-argv={}",
            where_it_ran.display(),
            argv.display()
        ),
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    let graph_dir = workspace
        .dir()
        .canonicalize()
        .expect("the graph's directory");
    let reported: Vec<std::path::PathBuf> = std::fs::read_to_string(&where_it_ran)
        .expect("the member's harness recorded its directory")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            std::path::Path::new(line.trim())
                .canonicalize()
                .expect("a directory the harness reported")
        })
        .collect();
    assert!(
        reported.contains(&graph_dir),
        "no side of the member ran in the directory the graph was given ({}): {reported:?}",
        graph_dir.display()
    );
    // And none of them ran in a directory this crate generated. Stated as its own
    // assertion because it is the failure: a harness in the run's own state
    // directory is one whose edits nobody is looking for.
    let state = workspace
        .state()
        .canonicalize()
        .expect("the state directory");
    for directory in &reported {
        assert!(
            !directory.starts_with(&state),
            "a side ran inside the run's generated state rather than the operator's \
             directory: {}",
            directory.display()
        );
    }

    // The same answer in the stream, where a supervisor reads it. Asserted equal
    // to what the harness reported rather than to a path this test rebuilt, so a
    // change that repoints one without the other fails here.
    let started = run.of_kind("member-started");
    assert_eq!(started.len(), 1, "{started:?}");
    let claimed = started[0]["payload"]["worktree"]
        .as_str()
        .expect("an in-process member names its worktree");
    assert_eq!(
        std::path::Path::new(claimed)
            .canonicalize()
            .expect("the published worktree exists"),
        graph_dir,
        "the member was published with a worktree that is not the graph's directory"
    );

    // And the pin held while it moved: the member's own `read-only` reached the
    // harness, not the decoy's `bypass`. Every side, not merely one — the judge
    // side has always had a `--config` of its own, so an `any` here would be
    // satisfied by the side that was never at risk and would pass with the agent
    // side's pin taken away.
    let recorded = std::fs::read_to_string(&argv).expect("the harness recorded its argv");
    let lines: Vec<&str> = recorded.lines().collect();
    assert!(
        lines.len() >= 2,
        "not every side reached the double: {recorded:?}"
    );
    for line in &lines {
        assert!(
            line.contains("--tools Read Grep Glob"),
            "a side ran under a config nobody named — discovery found the decoy \
             beside it instead of the member's own stamped file: {line}"
        );
    }
}

/// A run that names no `--dir` hands **both** member kinds the same default —
/// `.`, exactly as `member_dir` passes an unnamed directory through — and that
/// one string still resolves differently down the two paths.
///
/// The asymmetry is pre-existing and deliberately untouched: a relative `--cwd`
/// is resolved by whoever receives it, and a single-sided member's argv rides a
/// child spawned in the member's *scratch* while a two-party member's worktree is
/// read by an `oneharness` this process spawns without moving. `member_dir`
/// promises a member that named no directory behaves as it did before, and that
/// is what is pinned here; naming `--dir` is what makes the two agree. A relative
/// *member* `dir` has no such edge — it is made absolute before it is handed on.
#[test]
fn a_run_that_names_no_directory_hands_both_member_kinds_the_same_default() {
    let workspace = Workspace::new();
    let two_party = workspace.at("two-party.cwd");
    let single_sided = workspace.at("single-sided.cwd");
    // The binary is run with the workspace root as its working directory, which
    // is what an unnamed `.` resolves against for the side nothing moved.
    let launched_from = workspace.path().canonicalize().expect("the launch dir");
    let state = workspace
        .state()
        .canonicalize()
        .expect("the state directory");

    let recorded = |path: &std::path::PathBuf| {
        std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("{}: no directory recorded: {err}", path.display()))
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                std::path::Path::new(line.trim())
                    .canonicalize()
                    .expect("a directory the harness reported")
            })
            .collect::<Vec<_>>()
    };

    let two_party_run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &format!(
            "fake:complete-now write the thing. fake:record-cwd={}",
            two_party.display()
        ),
    ]);
    two_party_run.expect_code(0);
    // The value this crate decided, on the stream: the run's own default, passed
    // through as written rather than resolved into something nobody typed.
    assert_eq!(
        two_party_run.of_kind("member-started")[0]["payload"]["worktree"],
        serde_json::json!("."),
        "a two-party member in a run with no --dir was published with a directory \
         the run never named"
    );
    assert!(
        recorded(&two_party).contains(&launched_from),
        "a two-party member in a run with no --dir did not run where the run was \
         launched: {:?}",
        recorded(&two_party)
    );

    workspace.graph(&single_sided_graph(&fake_harness()));
    let single_run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &format!(
            "fake:complete-now write the thing. fake:record-cwd={}",
            single_sided.display()
        ),
    ]);
    single_run.expect_code(0);
    // A resolved directory, unlike the two-party member's `.` above: nothing
    // downstream resolves `RunRequest::cwd`, so the anchoring happens here.
    let published = single_run.of_kind("member-started")[0]["payload"]["worktree"]
        .as_str()
        .expect("a single-sided member names its worktree")
        .to_string();
    assert!(
        std::path::Path::new(&published).is_absolute(),
        "a directory nothing downstream resolves was published unresolved: \
         {published:?}"
    );
    // Compared canonically, because the two sides are spelled by different
    // parties: `state` is this test's own path resolved, while `published` is the
    // one the run built from `ONEAGENTGRAPH_STATE_DIR` as it was handed it. A
    // host whose temporary directory is a symlink spells those differently and
    // means the same directory — macOS's `/var` → `/private/var` is the case that
    // fails a string comparison here while nothing is wrong.
    let published = std::path::Path::new(&published)
        .canonicalize()
        .expect("the published worktree is a directory that exists");
    assert!(
        published.starts_with(&state),
        "the published worktree is not the member's scratch: {published:?}"
    );
    // And the harness really ran there. The two kinds differ, and the difference
    // is preserved rather than tidied away.
    let where_it_ran = recorded(&single_sided);
    assert_eq!(where_it_ran.len(), 1, "{where_it_ran:?}");
    assert!(
        where_it_ran[0].starts_with(&state),
        "a single-sided member's unnamed directory stopped meaning what it always \
         meant: {where_it_ran:?}"
    );
}
// llmlint: ignore-end[tests_mirror_real_usage]

/// Two members share the run's own task and differ only in what they are told to
/// do with it, while a third is handed it unchanged — read back from the harness
/// each one actually started.
///
/// The case a member's own `task` could not express. Replacing the run's prose
/// outright is right for a member whose job has nothing to do with the run, and
/// wrong for the pair this graph has: an orchestrator and a check-in that need the
/// *same* run context and opposite instructions about it. Until `{task}` there was
/// no way to say that — the context had to be restated by hand in the member's own
/// prose, or smuggled in through an environment variable, and the two copies drift
/// the first time somebody edits one.
///
/// Every compatibility guarantee the token rests on is asserted in the same run,
/// because they are only worth anything together: `plain` carries no task and is
/// handed the run's, `own_job` carries one naming no token and replaces it
/// outright, and `escaped` gets the one escape there is. What each member's
/// harness was really given is one line per member in one file — the double writes
/// the whole prompt it received, newlines and all — so a member handed somebody
/// else's job is visible as such rather than as an absence.
#[test]
fn members_share_the_runs_task_where_they_name_it_and_replace_it_where_they_do_not() {
    let workspace = Workspace::new();
    // One recording path per member: the double appends the whole prompt it was
    // given, and five members appending to one file interleave.
    let recorded = |member: &str| workspace.at(&format!("{member}.prompt"));
    let run_task = "fake:complete-now RUN CONTEXT: ship the retry.";
    let own = |member: &str, instruction: &str| {
        format!(
            "fake:complete-now {instruction} fake:record-prompt={}",
            recorded(member).display()
        )
    };
    let shared = |member: &str, instruction: &str| {
        format!(
            "{{task}}\n\n{instruction} fake:record-prompt={}",
            recorded(member).display()
        )
    };
    let orchestrator = shared(
        "orchestrator",
        "Drive this run to settlement, and nothing else.",
    );
    let check_in = shared(
        "check_in",
        "Report progress for the planner. Never drive the run.",
    );
    let own_job = own("own_job", "write one status update, and nothing else.");
    let escaped = own(
        "escaped",
        "mind the {{task}} escape, {other} braces, and a lone { — all prose.",
    );
    workspace.graph(&graph_with(
        concat!(
            "version: 4\nname: node-scope\n",
            "env: {}\n",
            "members:\n",
            "  orchestrator:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "  check_in:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "  plain:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "  own_job:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "  escaped:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            (
                "members.orchestrator.task".to_string(),
                orchestrator.clone(),
            ),
            ("members.check_in.task".to_string(), check_in.clone()),
            ("members.own_job.task".to_string(), own_job.clone()),
            ("members.escaped.task".to_string(), escaped.clone()),
        ],
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        run_task,
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    // What each member's harness was really given, read off the file that
    // harness wrote. The two members naming the token got the run's context and
    // their own instructions, and neither got the other's.
    let carried = |member: &str| {
        std::fs::read_to_string(recorded(member)).unwrap_or_else(|err| {
            panic!("member {member:?} recorded no prompt, so it was never given one: {err}")
        })
    };
    let context = "RUN CONTEXT: ship the retry";
    let driving = carried("orchestrator");
    assert!(driving.contains(context), "{driving}");
    assert!(
        driving.contains("Drive this run to settlement"),
        "{driving}"
    );
    assert!(
        !driving.contains("Report progress"),
        "a member was handed its sibling's job: {driving}"
    );
    let reporting = carried("check_in");
    assert!(reporting.contains(context), "{reporting}");
    assert!(reporting.contains("Report progress"), "{reporting}");
    assert!(
        !reporting.contains("Drive this run to settlement"),
        "a member was handed its sibling's job: {reporting}"
    );

    // A task naming no token still replaces the run's outright, and the one
    // escape reaches the harness as the text it names rather than as the run's
    // task in braces.
    let replaced = carried("own_job");
    assert!(replaced.contains("write one status update"), "{replaced}");
    assert!(
        !replaced.contains(context),
        "a member whose task names no token was still handed the run's: {replaced}"
    );
    let literal = carried("escaped");
    for text in [
        "mind the {task} escape",
        "{other} braces",
        "a lone { — all prose",
    ] {
        assert!(
            literal.contains(text),
            "{text:?} did not reach the harness as the text it names: {literal}"
        );
    }
    assert!(
        !literal.contains(context),
        "an escaped token interpolated the run's task anyway: {literal}"
    );

    // And the prompt each member's turn really ran, byte for byte, read off
    // oneharness's **own** report — its `prompt` field, stored where each
    // member's settle says it is. That is where `plain` (the member carrying no
    // task at all) is answered for: it is handed the run's task exactly as the
    // run was given it, and unlike the four above it writes no recording of its
    // own to be read back from.
    let expected: BTreeMap<&str, String> = [
        ("orchestrator", orchestrator.replace("{task}", run_task)),
        ("check_in", check_in.replace("{task}", run_task)),
        ("plain", run_task.to_string()),
        ("own_job", own_job),
        ("escaped", escaped.replace("{{task}}", "{task}")),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        run.of_kind("member-started").len(),
        expected.len(),
        "{:?}",
        run.kinds()
    );
    for (member, prompt) in &expected {
        assert_eq!(
            stored_report(&run, member)["prompt"],
            serde_json::json!(prompt),
            "member {member:?} ran a prompt nobody wrote"
        );
    }
}

/// A two-party member's own `task` takes the run's on the same terms, through the
/// real onejudge engine.
///
/// One field, one rule, whichever kind of member carries it — and a two-party
/// member reaches its harness by a different road entirely: no argv, an effective
/// config this crate writes, and onejudge's own run driver in this process. What
/// the harness was handed is read back from the harness, because that road is
/// where a task the engine never carried would be lost.
#[test]
fn a_two_party_member_takes_the_runs_task_where_it_names_it() {
    let workspace = Workspace::new();
    let prompts = workspace.at("prompts.txt");
    workspace.graph(&graph_with(
        // Version 4 is the schema in which `{task}` is a token rather than the
        // six characters it has always been, and the default graph is written
        // against version 1.
        &two_party_graph(&fake_harness(), NO_ENV).replace("version: 1", "version: 4"),
        &[(
            "members.worker.task",
            "{task}\n\nJudge it against that context, and nothing else.",
        )],
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &format!(
            "fake:complete-now RUN CONTEXT: ship the retry. fake:record-prompt={}",
            prompts.display()
        ),
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    let recorded = std::fs::read_to_string(&prompts).expect("the agent recorded its prompt");
    let carried = recorded
        .lines()
        .find(|line| line.contains("Judge it against that context"))
        .unwrap_or_else(|| panic!("no side was given the member's own task:\n{recorded}"));
    assert!(
        carried.contains("RUN CONTEXT: ship the retry"),
        "the token reached the engine unexpanded, so the member lost the run's context: {carried}"
    );
    assert!(
        !carried.contains("{task}"),
        "an unexpanded token reached the harness: {carried}"
    );
}

/// Under the schema before the token, `{task}` in a member's task is the six
/// characters it has always been.
///
/// The compatibility guarantee the version gate exists for, driven rather than
/// argued: a member task is prose, and a document written against version 3 that
/// happens to contain those characters says them. A run of that document must
/// hand the harness exactly what the document says — the alternative is this
/// crate silently rewriting prose somebody already shipped.
#[test]
fn a_task_token_is_literal_prose_under_the_schema_before_it() {
    let workspace = Workspace::new();
    let prompts = workspace.at("prompts.txt");
    workspace.graph(&graph_with(
        concat!(
            "version: 3\nname: node-scope\n",
            "env: {}\n",
            "members:\n  check_in:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            (
                "members.check_in.task".to_string(),
                format!(
                    "fake:complete-now mind the {{task}} in this sentence. fake:record-prompt={}",
                    prompts.display()
                ),
            ),
        ],
    ));
    workspace
        .run(&[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now RUN CONTEXT: ship the retry.",
            "--dir",
            &workspace.dir().display().to_string(),
        ])
        .expect_code(0);

    let recorded = std::fs::read_to_string(&prompts).expect("the member recorded its prompt");
    assert!(
        recorded.contains("mind the {task} in this sentence"),
        "a version 3 document's prose was rewritten: {recorded}"
    );
    assert!(
        !recorded.contains("RUN CONTEXT"),
        "the token expanded under a schema that never had it: {recorded}"
    );
}

/// The token takes the run's task from `--task-file` as readily as from `--task`.
///
/// The two flags are one task by the time a member is built, and a run whose
/// context is long enough to want interpolating is exactly the run whose author
/// reached for a file to hold it.
#[test]
fn a_task_token_takes_a_run_task_that_came_from_a_file() {
    let workspace = Workspace::new();
    let prompts = workspace.at("prompts.txt");
    workspace.write(
        "task.txt",
        "fake:complete-now RUN CONTEXT: ship the retry, from a file.\n",
    );
    workspace.graph(&graph_with(
        concat!(
            "version: 4\nname: node-scope\n",
            "env: {}\n",
            "members:\n  check_in:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            (
                "members.check_in.task".to_string(),
                format!(
                    "{{task}}\n\nReport progress. fake:record-prompt={}",
                    prompts.display()
                ),
            ),
        ],
    ));
    workspace
        .run(&[
            "run",
            "./graph.yaml",
            "--task-file",
            "./task.txt",
            "--dir",
            &workspace.dir().display().to_string(),
        ])
        .expect_code(0);

    let recorded = std::fs::read_to_string(&prompts).expect("the member recorded its prompt");
    assert!(
        recorded.contains("RUN CONTEXT: ship the retry, from a file."),
        "the token expanded to nothing for a run whose task came from a file: {recorded}"
    );
    assert!(
        recorded.contains("Report progress."),
        "the member lost its own prose: {recorded}"
    );
}

/// A member naming `{task}` in a run that supplied none is handed the rest of its
/// own prose, and the run is not refused for it.
///
/// The taskless run is the one a graph of self-describing members makes, and a
/// token that demanded a `--task` would take that shape away — a check-in member
/// paced by a schedule would suddenly require prose nobody has typed.
#[test]
fn a_task_token_in_a_run_that_supplied_no_task_expands_to_nothing() {
    let workspace = Workspace::new();
    let prompts = workspace.at("prompts.txt");
    workspace.graph(&graph_with(
        concat!(
            "version: 4\nname: node-scope\n",
            "env: {}\n",
            "members:\n  check_in:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            (
                "members.check_in.task".to_string(),
                format!(
                    "{{task}}fake:complete-now write one status update. fake:record-prompt={}",
                    prompts.display()
                ),
            ),
        ],
    ));
    workspace
        .run(&[
            "run",
            "./graph.yaml",
            "--dir",
            &workspace.dir().display().to_string(),
        ])
        .expect_code(0);
    let recorded = std::fs::read_to_string(&prompts).expect("the member recorded its prompt");
    assert!(
        recorded.contains("write one status update"),
        "the member never ran its own task: {recorded}"
    );
    assert!(
        !recorded.contains("{task}"),
        "an unexpanded token reached the harness: {recorded}"
    );
}

/// A member whose *whole* task is the token, in a run that supplied none, is
/// launched with an empty prompt rather than with a `--prompt` that has no value.
///
/// The argv edge the token opens. This crate used to drop empty arguments on the
/// way to `oneharness run`, which can only ever remove a *value* — the argv holds
/// no optional flags — leaving `--prompt` sitting next to the flag that followed
/// it, so oneharness would read `--events` as the prompt and the member would run
/// something nobody wrote. Asserted on the argv the member was launched with,
/// because the failure was invisible in the outcome: a run that spends a turn on
/// the wrong prompt exits 0.
#[test]
fn a_member_whose_whole_task_is_the_token_is_launched_with_an_empty_prompt() {
    let workspace = Workspace::new();
    workspace.graph(&graph_with(
        concat!(
            "version: 4\nname: node-scope\n",
            "env: {}\n",
            "members:\n  check_in:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            ("members.check_in.task".to_string(), "{task}".to_string()),
        ],
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);

    let started = run.of_kind("member-started");
    assert_eq!(started.len(), 1, "{started:?}");
    // The empty prompt is carried as a prompt rather than dropped: oneharness's
    // report echoes exactly what it ran, so an empty one that had been elided on
    // the way in would show up here as a run of something else.
    assert_eq!(
        stored_report(&run, "check_in")["prompt"],
        serde_json::json!(""),
        "the empty prompt did not reach the turn as one"
    );
    // And the member really was launched with it: what an empty prompt means is
    // oneharness's and the harness's to decide, and this run reaches that decision
    // rather than failing on an argv this crate malformed.
    run.expect_code(0);
    assert!(
        run.of_kind("member-settled")
            .iter()
            .any(|event| labels(event)["member"] == "check_in"),
        "the member launched with an empty prompt never settled: {}",
        run.stdout
    );
}

/// A graph whose every member carries its own job runs with **no `--task` at
/// all** — and the same graph without that job is refused, saying so.
///
/// The pacemaker's case, and the reason the field is required rather than
/// convenient. A member that exists to write one status update on a schedule has
/// no relationship to whatever prose the run was launched with; a graph of such
/// members should not have to be handed a task it will never use, and until it
/// could hold one there was no way to express that. The refusal is asserted
/// beside it because it is what proves the member's own task is what satisfied
/// the run, rather than a run that never needed one.
///
/// The directory here is **absolute**, the form used as written — the shape a
/// member working in a scratch that is nowhere near the graph's own directory
/// actually takes.
#[test]
fn a_graph_whose_members_carry_their_own_jobs_needs_no_task_of_its_own() {
    let workspace = Workspace::new();
    let elsewhere = workspace.at("pacemaker-scratch");
    std::fs::create_dir_all(&elsewhere).expect("the member's own directory");
    let recorded = workspace.at("pacemaker.prompt");
    let where_it_ran = workspace.at("pacemaker.cwd");

    const SKELETON: &str = concat!(
        "version: 3\nname: node-scope\n",
        "env: {}\n",
        "members:\n",
        "  check_in:\n    kind: oneharness\n",
        "    oneharness_config: ./oneharness.toml\n",
    );
    let job: &[(&str, String)] = &[
        (FAKE_HARNESS_KEY, fake_harness()),
        ("members.check_in.dir", elsewhere.display().to_string()),
    ];
    let graph = graph_with(SKELETON, job);
    let with_own_task = graph_with(
        SKELETON,
        &[
            job[0].clone(),
            job[1].clone(),
            (
                "members.check_in.task",
                format!(
                    "fake:complete-now write one status update. \
                     fake:record-prompt={} fake:record-cwd={}",
                    recorded.display(),
                    where_it_ran.display(),
                ),
            ),
        ],
    );

    workspace.graph(&with_own_task);
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);
    assert!(
        std::fs::read_to_string(&recorded)
            .unwrap_or_default()
            .contains("write one status update"),
        "a member's own task did not reach its harness when the run supplied none"
    );
    let reported = std::fs::read_to_string(&where_it_ran)
        .expect("the member's own directory")
        .trim()
        .to_string();
    assert_eq!(
        std::path::Path::new(&reported)
            .canonicalize()
            .expect("the reported member directory"),
        elsewhere.canonicalize().expect("canonical"),
        "an absolute member directory must be used exactly as written"
    );

    // The same graph with the member's job taken away: nothing supplies a task
    // now, and the run says which two things could.
    workspace.graph(&graph);
    let refused = workspace.run(&[
        "run",
        "./graph.yaml",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    refused.expect_code(2);
    assert!(
        refused.stderr.contains("no task"),
        "a run with nothing to do was not refused by name: {}",
        refused.stderr
    );
}

/// A member's own `task` and `dir` carry a Windows path exactly, and the member
/// is handed what the document said.
///
/// The regression for the two journeys above, which could not hold it
/// themselves: they formatted a path into a double-quoted YAML scalar, and `\U`
/// in `C:\Users\…` opens an eight-digit unicode escape there, so the parser
/// refused the document and both runs exited 2 without reaching what they test.
/// The path here is a Windows one whatever host is reading this — the defect was
/// a serialization question wearing a platform's clothes, so proving it needs no
/// platform.
#[test]
fn a_member_s_own_job_carries_a_windows_path_exactly() {
    let workspace = Workspace::new();
    // The shape of a real one: a temporary directory under a user profile, which
    // is where every journey's paths live on that platform.
    let windows = r"C:\Users\runneradmin\AppData\Local\Temp\.tmpQ1u9";
    let task = format!(r"fake:complete-now write one update. fake:seen={windows}\check-in.prompt");
    let skeleton = concat!(
        "version: 3\nname: node-scope\n",
        "env: {}\n",
        "members:\n  check_in:\n    kind: oneharness\n",
        "    oneharness_config: ./oneharness.toml\n",
    );

    // A member's directory, which the run reads before it launches anything.
    // This is the failure as it was observed: `oneagentgraph: invalid config:
    // ./graph.yaml: did not find expected hexadecimal number`, exit 2.
    workspace.graph(&graph_with(
        skeleton,
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            ("members.check_in.dir", windows.to_string()),
        ],
    ));
    workspace.run(&["validate", "./graph.yaml"]).expect_code(0);

    // And a member's task, all the way to the harness: what the stream says it
    // was given is the prose that was written, escape sequence and all. The
    // directory is the run's here, because a Windows one names nothing on the
    // host this runs on — parsing it is the half that can be held everywhere.
    workspace.graph(&graph_with(
        skeleton,
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            ("members.check_in.task", task.clone()),
        ],
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: the run's own task, which this member does not use",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    let started = run.of_kind("member-started");
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(
        stored_report(&run, "check_in")["prompt"],
        serde_json::json!(task),
        "the member ran something other than the task the document carried"
    );
}

/// A single-sided member whose harness exits without publishing a report dies as
/// a provider failure, carrying the cause and the detail — and **none** of the
/// three facts a child process leaves behind, because this member is not one.
///
/// The pair with the two-party provider failure in `tests/e2e/liveness.rs` is the
/// point, and the conversion is what made the two agree: `docs/contract.md`
/// scopes `exit_code`, `disposition` and `stderr_tail` to "a member that was one",
/// and no member is now, so a consumer reads `cause` and `detail` whichever kind
/// died. What the harness underneath actually did is still there — in the report
/// oneharness returned, which is where its own accounting belongs.
#[test]
fn a_single_sided_member_that_crashes_dies_with_a_cause_and_no_process_facts() {
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
    // `unclassified`, not a category invented from an exit code: oneharness owns
    // classification, and the four kinds it has that `cause` cannot spell are a
    // contract change rather than a partial map — see
    // `docs/oneharness-library.md`.
    assert_eq!(payload["cause"], serde_json::json!("unclassified"));
    for absent in ["exit_code", "disposition", "stderr_tail"] {
        assert!(payload.get(absent).is_none(), "{absent}: {payload}");
    }
    // The detail is the run's own failure summary, which names the harness that
    // did not succeed — the evidence an operator reads, in place of the stderr
    // tail a spawned turn left behind.
    let detail = payload["detail"].as_str().unwrap_or_default();
    assert!(!detail.is_empty(), "a death with no evidence: {payload}");
    assert!(
        detail.contains("claude-code"),
        "the death named no harness: {payload}"
    );
    assert!(
        detail.len() <= 4096,
        "the detail outgrew its documented bound"
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
            "name: lead\nsystem_prompt: |\n  Role marker: you lead.\n",
            "user:\n  persona: |\n    Supervisor marker: push hard.\n",
        ),
    );
    workspace.graph(
        &two_party_graph(&fake_harness(), NO_ENV)
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

/// A base config's `done_when` is the review bar an operator centralizes for
/// every dispatch, so a persona bringing its own adds a bar rather than removing
/// that one — and the effective config the member is handed carries both.
///
/// Read off the generated `onejudge.yaml` because that document *is* what the
/// dispatch received: a bar that never reached the file never reached the
/// supervisor either, whatever the merge computed in memory.
#[test]
fn a_personas_own_bar_is_enforced_beside_the_bases_rather_than_instead_of_it() {
    /// One dispatch's own `onejudge.yaml`, read back out of the run's state
    /// directory: what the merge computed matters only insofar as it reached the
    /// document the member ran on.
    fn effective_config(base: &str, extra: &str) -> String {
        let workspace = Workspace::new();
        workspace.write("base.yaml", base);
        workspace.write(
            "roles/lead.yaml",
            &format!(
                concat!(
                    "name: lead\nsystem_prompt: |\n  Role marker: you lead.\n",
                    "user:\n  persona: |\n    Supervisor marker: push hard.\n",
                    "  done_when: \"Role bar: every finding cites the code it names\"\n{}",
                ),
                extra
            ),
        );
        workspace.graph(
            &two_party_graph(&fake_harness(), NO_ENV)
                .replace("persona: engineer", "persona: ./roles/lead.yaml"),
        );
        workspace
            .run_task("fake:complete-now: the bars")
            .expect_code(0);
        workspace.member_file("worker", "onejudge.yaml")
    }

    let composed = effective_config(BASE, "");
    assert!(
        composed.contains("the task is complete"),
        "the base's review bar was dropped by the persona: {composed}"
    );
    assert!(
        composed.contains("Role bar: every finding cites the code it names"),
        "{composed}"
    );

    // And a persona that genuinely must stand in for the shared bar says so,
    // which is the one way the base's bar leaves the effective config.
    let replaced = effective_config(BASE, "  done_when_replaces_base: true\n");
    assert!(
        !replaced.contains("the task is complete"),
        "an explicit replacement must be the only bar: {replaced}"
    );
    assert!(
        replaced.contains("Role bar: every finding cites the code it names"),
        "{replaced}"
    );

    // A base whose bar is blank has no bar to compose with, and the persona's
    // stands alone rather than being numbered against nothing — the same reading
    // a base that names none at all gets.
    let blank = effective_config(&BASE.replace("\"the task is complete\"", "'   '"), "");
    assert!(
        !blank.contains("Both of these must hold"),
        "one bar must not be handed over as two: {blank}"
    );
    assert!(
        blank.contains("Role bar: every finding cites the code it names"),
        "{blank}"
    );
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
        &[("ONEAGENTGRAPH_TEST_MARKER", "${E2E_SOURCE}/leaf")],
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
    workspace.graph(
        &two_party_graph(&fake_harness(), NO_ENV).replace("mode: bypass", "mode: read-only"),
    );
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
    workspace.graph(&graph_with(
        concat!(
            "version: 2\nname: node-scope\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      command: [the provider below]\n",
            "    mode: bypass\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            // The one value here that is not a graph key: an element of a
            // sequence, addressed by its index.
            ("members.worker.judge.command.0", fake_provider()),
        ],
    ));
    // A base carrying evals and an assessment reaches every operation the
    // protocol has, so the whole command-provider surface is driven rather than
    // just the completion decision.
    workspace.write(
        "base.yaml",
        concat!(
            "provider:\n  kind: oneharness\n",
            "system_prompt: |\n  Standing bar: verify before you claim done.\n",
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

/// A persona a member cannot run under refuses the run, naming what is wrong —
/// before a paid turn is spent on it.
///
/// Ported from `test_dispatch_unknown_persona_raises` and
/// `test_dispatch_rejects_unsafe_persona_names`.
#[test]
fn an_unusable_persona_refuses_the_run_before_anything_starts() {
    let workspace = Workspace::new();
    let cases = [
        ("./roles/nowhere.yaml", "cannot read"),
        // A key onejudge does not have, refused by onejudge's own schema: this
        // crate keeps no second copy of it to be stricter than.
        ("./roles/typo.yaml", "unknown field `system_promt`"),
        // A key the member's own launch decides, refused where it is written
        // rather than overwritten on the way to onejudge.
        ("./roles/provider.yaml", "decide its provider"),
        // Replacing the operator's shared review bar with nothing at all is the
        // silent drop said out loud, and is refused as the mistake it is.
        (
            "./roles/replaces-nothing.yaml",
            "names nothing to replace the base's bar with",
        ),
    ];
    workspace.write("roles/typo.yaml", "system_promt: typo\n");
    workspace.write("roles/provider.yaml", "provider:\n  kind: command\n");
    workspace.write(
        "roles/replaces-nothing.yaml",
        "system_prompt: r\nuser:\n  persona: p\n  done_when_replaces_base: true\n",
    );
    for (reference, expected) in cases {
        workspace.graph(
            &two_party_graph(&fake_harness(), NO_ENV)
                .replace("persona: engineer", &format!("persona: {reference}")),
        );
        let run = workspace.run_task("fake:complete-now: never gets here");
        run.expect_code(2);
        assert!(run.stderr.contains(expected), "{reference}: {}", run.stderr);
    }
}

/// The spelling this crate used to define does not load when a graph runs
/// either, and the refusal is the whole migration: it names the file, the key it
/// refused, and the onejudge field to write instead.
///
/// A member is where a persona is *used*, so this is the path an operator hits
/// first — and there is nothing behind it. No alias, no flag, no environment
/// variable: the run refuses, and produces no member at all.
#[test]
fn the_previous_persona_spelling_refuses_the_run_and_says_how_to_repair_it() {
    let workspace = Workspace::new();
    workspace.write(
        "roles/lead.yaml",
        concat!(
            "agent:\n  name: lead\n  instructions: |\n    Role marker: you lead.\n",
            "user:\n  persona: |\n    Supervisor marker: push hard.\n",
        ),
    );
    workspace.graph(
        &two_party_graph(&fake_harness(), NO_ENV)
            .replace("persona: engineer", "persona: ./roles/lead.yaml"),
    );

    let refused = workspace.run_task("fake:complete-now: never gets here");
    refused.expect_code(2);
    for named in [
        "roles/lead.yaml",
        "`agent.instructions`",
        "`system_prompt`",
        "`agent.name`",
        "top-level `name`",
        "no deprecation period",
    ] {
        assert!(
            refused.stderr.contains(named),
            "{named}: {}",
            refused.stderr
        );
    }
    assert!(
        refused.stdout.is_empty(),
        "a refusal must not read as an event stream: no member was produced"
    );

    // A base config is a onejudge config for the same reason, so the preamble
    // this crate used to read out of an `agent:` block there is refused too,
    // rather than translated a second time on the other side of the merge.
    workspace.write(
        "roles/lead.yaml",
        "system_prompt: |\n  Role marker: you lead.\n",
    );
    workspace.write(
        "base.yaml",
        &BASE.replace(
            "system_prompt: |\n  Standing bar",
            "agent:\n  instructions: |\n    Standing bar",
        ),
    );
    let base = workspace.run_task("fake:complete-now: never gets here");
    base.expect_code(2);
    assert!(base.stderr.contains("base.yaml"), "{}", base.stderr);
    assert!(base.stderr.contains("`system_prompt`"), "{}", base.stderr);
}

/// A graph whose member resolves its `persona` name against a catalog at
/// `catalog` — a path the *graph document* is the base for, not the directory
/// the CLI was invoked from.
fn catalogued_graph(catalog: &str, persona: &str) -> String {
    graph_with(
        &format!(
            concat!(
                "version: 6\nname: node-scope\n",
                "env: {{}}\n",
                "personas: {}\n",
                "members:\n  worker:\n    kind: onejudge\n",
                "    base_config: ./base.yaml\n    persona: {}\n",
                "    agent:\n      oneharness_config: ./oneharness.toml\n",
                "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
                "    mode: bypass\n",
            ),
            catalog, persona
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
    )
}

// llmlint: ignore-block[tests_mirror_real_usage] the three journeys below read the
// prompt file the doubled harness recorded, at the observation point and for the
// reason the block above states: which persona document reached the agent is not
// on the stream, and a member dispatched under the wrong one settles identically.
/// A graph's own persona catalog is dispatchable by name — the slash-qualified
/// one included — and the role in that file is what the agent is actually run
/// under.
///
/// The evidence an operator has that a catalog works is the prompt the harness
/// received, so that is what this reads: a catalog nothing maps a name onto is
/// inert however well its files parse.
#[test]
fn a_graph_local_persona_catalog_is_dispatchable_by_name() {
    let workspace = Workspace::new();
    workspace.write(
        "personas/crozier/crozier-corpus.yaml",
        concat!(
            "system_prompt: |\n  Catalog marker: mind the corpus.\n",
            "user:\n  persona: |\n    Catalog supervisor: check the citations.\n",
        ),
    );
    workspace.graph(&catalogued_graph("./personas", "crozier/crozier-corpus"));

    let record = workspace.at("prompts.txt");
    let run = workspace.run_task(&format!(
        "fake:complete-now: catalogued fake:record-prompt={}",
        record.display()
    ));
    run.expect_code(0);

    let delivered = std::fs::read_to_string(&record).expect("prompts");
    assert!(
        delivered.contains("Catalog marker: mind the corpus."),
        "the catalog's role never reached the agent: {delivered}"
    );
    assert!(
        delivered.contains("Catalog supervisor: check the citations."),
        "{delivered}"
    );
    assert!(
        delivered.contains("Standing bar: verify before you claim done."),
        "{delivered}"
    );
    assert_eq!(
        labels(&run.of_kind("member-started")[0])["persona"],
        "crozier-corpus"
    );

    // A catalog does not take the shipped personas away: a name it does not hold
    // resolves to the one this crate ships, which is what every graph written
    // before catalogs existed depends on.
    workspace.graph(&catalogued_graph("./personas", "reviewer"));
    let shipped = workspace.run_task("fake:complete-now: still shipped");
    shipped.expect_code(0);
    assert_eq!(
        labels(&shipped.of_kind("member-started")[0])["persona"],
        "reviewer"
    );
}

/// A relative catalog is resolved against the graph document, not the directory
/// the CLI happened to be invoked from — and an absolute one is used as written.
///
/// The graph and its refs sit one level down while the run is started from the
/// workspace root, with a decoy catalog at that root carrying a persona of the
/// same name. A run resolving against the process's own directory would take the
/// decoy, which is how a graph fetched or vendored from elsewhere ends up
/// running under a neighbour's personas.
#[test]
fn a_catalog_is_resolved_against_the_graph_document_and_an_absolute_one_as_written() {
    let workspace = Workspace::new();
    let persona = |marker: &str| {
        format!(
            concat!(
                "system_prompt: |\n  {} role.\n",
                "user:\n  persona: |\n    {} supervisor.\n",
            ),
            marker, marker
        )
    };
    workspace.write("personas/lead.yaml", &persona("Decoy"));
    workspace.write("node/personas/lead.yaml", &persona("Beside the graph"));
    workspace.write("elsewhere/lead.yaml", &persona("Named absolutely"));
    for name in ["base.yaml", "oneharness.toml", "oneharness.judge.toml"] {
        workspace.write(&format!("node/{name}"), &workspace.read(name));
    }

    let absolute = workspace.at("elsewhere");
    for (catalog, marker) in [
        ("./personas", "Beside the graph"),
        (&absolute.display().to_string(), "Named absolutely"),
    ] {
        workspace.write("node/graph.yaml", &catalogued_graph(catalog, "lead"));
        let record = workspace.at("prompts.txt");
        workspace
            .run(&[
                "run",
                "./node/graph.yaml",
                "--task",
                &format!(
                    "fake:complete-now: which catalog fake:record-prompt={}",
                    record.display()
                ),
                "--dir",
                &workspace.dir().display().to_string(),
            ])
            .expect_code(0);
        let delivered = std::fs::read_to_string(&record).expect("prompts");
        assert!(
            delivered.contains(&format!("{marker} role.")),
            "{catalog}: {delivered}"
        );
        assert!(!delivered.contains("Decoy role."), "{catalog}: {delivered}");
        std::fs::remove_file(&record).expect("the recorded prompts");
    }
}

/// A path that names its own root but carries **no drive** — `/graphs/api` — is
/// read from where its author wrote it, at each of the three surfaces a graph
/// names one on: a path-shaped config ref, a persona catalog, and a base config's
/// `skill:`.
///
/// The refusal is the whole observation: a directory at the filesystem root is not
/// one a journey can create, and a path re-rooted the way `Path::join` re-roots
/// this one on Windows arrives back with a drive on the front.
#[test]
fn a_path_that_names_its_own_root_is_read_from_there_and_not_from_under_this_run() {
    let workspace = Workspace::new();

    // Every path-shaped ref in a graph resolves through one function, so a ref is
    // the widest of the three.
    workspace.graph(
        &two_party_graph(&fake_harness(), NO_ENV).replace("./base.yaml", "/graphs/api/base.yaml"),
    );
    let refused = workspace.run_task("fake:complete-now: never gets a base config");
    refused.expect_code(2);
    assert!(
        refused.stderr.contains("cannot read /graphs/api/base.yaml"),
        "{}",
        refused.stderr
    );

    // A persona catalog, which is a directory rather than a file and so is
    // refused by being unreadable rather than unfound.
    workspace.graph(&catalogued_graph("/graphs/api", "lead"));
    let refused = workspace.run_task("fake:complete-now: never finds a catalog");
    refused.expect_code(2);
    assert!(
        refused.stderr.contains("catalog (/graphs/api)"),
        "{}",
        refused.stderr
    );

    // A base config's own `skill:`, which is anchored twice — once into the
    // effective config this crate writes, and again when onejudge's plan is
    // resolved out of that copy. The refusal is onejudge's, on the stream, so
    // this is the member dying rather than the graph refusing to start.
    workspace.graph(&two_party_graph(&fake_harness(), NO_ENV));
    workspace.write("base.yaml", &format!("{BASE}skill: /graphs/api\n"));
    let died = workspace.run_task("fake:complete-now: never loads a skill");
    died.expect_code(1);
    let events = died.of_kind("member-died");
    assert_eq!(events.len(), 1, "{events:?}");
    let detail = events[0]["payload"]["detail"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(detail.contains("skill `/graphs/api`"), "{detail}");
}

/// A local persona and a shipped one of the same name is refused before the
/// graph starts, rather than resolved to the shipped one behind the operator's
/// back — and naming the file by path is the way to say which was meant.
#[test]
fn a_persona_name_in_both_catalogs_refuses_the_run() {
    let workspace = Workspace::new();
    workspace.write(
        "personas/reviewer.yaml",
        concat!(
            "system_prompt: |\n  Local marker: our own reviewer.\n",
            "user:\n  persona: |\n    Local supervisor: our own bar.\n",
        ),
    );
    workspace.graph(&catalogued_graph("./personas", "reviewer"));

    let refused = workspace.run_task("fake:complete-now: never gets here");
    refused.expect_code(2);
    assert!(refused.stderr.contains("names both"), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("reviewer.yaml"),
        "{}",
        refused.stderr
    );
    assert!(
        refused.stdout.is_empty(),
        "a refusal must not read as an event stream"
    );

    // The explicit selection: a path ref reaches the operator's file whatever it
    // is called, and the run goes through.
    workspace.graph(&catalogued_graph("./personas", "./personas/reviewer.yaml"));
    let record = workspace.at("prompts.txt");
    workspace
        .run_task(&format!(
            "fake:complete-now: ours fake:record-prompt={}",
            record.display()
        ))
        .expect_code(0);
    let delivered = std::fs::read_to_string(&record).expect("prompts");
    assert!(
        delivered.contains("Local marker: our own reviewer."),
        "{delivered}"
    );
}

// llmlint: ignore-end[tests_mirror_real_usage]

/// A catalog lookup that could not have meant what it says is refused before the
/// graph starts, with both catalogs named — never resolved to whatever happens
/// to be reachable.
#[test]
fn a_catalog_lookup_that_cannot_be_honoured_refuses_the_run() {
    let workspace = Workspace::new();
    workspace.write(
        "personas/ours.yaml",
        "system_prompt: ours\nuser:\n  persona: ours\n",
    );
    std::fs::create_dir_all(workspace.at("personas/adirectory.yaml")).expect("not a persona");
    for (graph, expected) in [
        // A name neither catalog holds. Read as a *file* it would be a path
        // nobody wrote, so it says where it looked and what it found instead.
        (
            catalogued_graph("./personas", "crozier/crozier-corpus"),
            "holds no crozier/crozier-corpus.yaml",
        ),
        // A catalog root that is not there at all: without this, a typo in the
        // path would send every name straight back to the shipped personas —
        // the silent shadowing the catalog exists to end.
        (
            catalogued_graph("./persona", "ours"),
            "is not a directory this run can read",
        ),
        // And one that is there and is not a catalog, which is the same
        // mistake reached by the other half of the same typo.
        (
            catalogued_graph("./base.yaml", "ours"),
            "is not a directory this run can read",
        ),
        // And a broken catalog no member happens to look in is refused all the
        // same, where it was written rather than a dispatch later when somebody
        // gives a member a name.
        (
            graph_with(
                concat!(
                    "version: 6\nname: node-scope\n",
                    "env: {}\n",
                    "personas: ./persona\n",
                    "members:\n  worker:\n    kind: onejudge\n",
                    "    base_config: ./base.yaml\n    persona: ./personas/ours.yaml\n",
                    "    agent:\n      oneharness_config: ./oneharness.toml\n",
                    "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
                    "    mode: bypass\n",
                ),
                &[(FAKE_HARNESS_KEY, fake_harness())],
            ),
            "is not a directory this run can read",
        ),
        // An entry of that name which is not a persona file is not a persona:
        // the catalog holds none by that name, and the answer says so rather
        // than trying to read a directory as a document.
        (
            catalogued_graph("./personas", "adirectory"),
            "holds no adirectory.yaml",
        ),
    ] {
        workspace.graph(&graph);
        let refused = workspace.run_task("fake:complete-now: never gets here");
        refused.expect_code(2);
        assert!(
            refused.stderr.contains(expected),
            "{graph}: {}",
            refused.stderr
        );
        assert!(
            refused.stdout.is_empty(),
            "a refusal must not read as an event stream"
        );
    }
}

/// A single-sided member resolves its persona out of the same catalog, and the
/// same collision refusal applies to it.
///
/// The two member kinds share one resolution, and a member with no judge is the
/// one whose persona is nothing *but* the label on its events — so the label is
/// where this reads the answer.
#[test]
fn a_single_sided_member_takes_its_persona_from_the_catalog_too() {
    let workspace = Workspace::new();
    let single_sided = |persona: &str| {
        graph_with(
            &format!(
                concat!(
                    "version: 6\nname: node-scope\n",
                    "env: {{}}\n",
                    "personas: ./personas\n",
                    "members:\n  reporter:\n    kind: oneharness\n",
                    "    oneharness_config: ./oneharness.toml\n    persona: {}\n",
                ),
                persona
            ),
            &[(FAKE_HARNESS_KEY, fake_harness())],
        )
    };
    workspace.write(
        "personas/crozier/crozier-corpus.yaml",
        "system_prompt: corpus role\nuser:\n  persona: corpus supervisor\n",
    );
    workspace.graph(&single_sided("crozier/crozier-corpus"));

    let run = workspace.run_task("fake:complete-now: catalogued and single sided");
    run.expect_code(0);
    assert_eq!(
        labels(&run.of_kind("member-started")[0])["persona"],
        "crozier-corpus"
    );

    workspace.write(
        "personas/reviewer.yaml",
        "system_prompt: ours\nuser:\n  persona: ours\n",
    );
    workspace.graph(&single_sided("reviewer"));
    let refused = workspace.run_task("fake:complete-now: never gets here");
    refused.expect_code(2);
    assert!(refused.stderr.contains("names both"), "{}", refused.stderr);
}

/// A catalog entry the filesystem will not describe is reported, not read as a
/// name the catalog does not hold.
///
/// The distinction is the whole of why the lookup asks about *absence* rather
/// than trusting a boolean: a persona that is there and unreadable, read as
/// absent, dispatches the member under a shipped role instead of saying what
/// went wrong. A symlink that points at itself is the one way to make a
/// filesystem refuse a stat for every user, including root — a permission bit
/// proves nothing on a runner that ignores it.
#[cfg(unix)]
#[test]
fn a_catalog_entry_that_cannot_be_described_is_reported_rather_than_missed() {
    let workspace = Workspace::new();
    std::fs::create_dir_all(workspace.at("personas")).expect("a catalog");
    std::os::unix::fs::symlink("lead.yaml", workspace.at("personas/lead.yaml"))
        .expect("a symlink to itself");
    workspace.graph(&catalogued_graph("./personas", "lead"));

    let refused = workspace.run_task("fake:complete-now: never gets here");
    refused.expect_code(2);
    assert!(
        refused.stderr.contains("from this graph's catalog"),
        "{}",
        refused.stderr
    );
    assert!(refused.stderr.contains("lead.yaml"), "{}", refused.stderr);
}

/// A persona carrying nothing but the role runs a member whose judge is a
/// `command` provider — the case that has no simulated user at all.
///
/// This is what a persona being a onejudge config fragment buys: onejudge is
/// content with a config that names no `user`, so this crate is too. An external
/// judge decides completion, so a simulated-user bar here would be a review bar
/// nothing reads — and the journey asserts none was invented on the member's
/// behalf, by reading the effective config the dispatch was actually handed.
#[test]
fn a_role_only_persona_runs_a_member_whose_judge_is_a_command() {
    let workspace = Workspace::new();
    workspace.graph(&graph_with(
        concat!(
            "version: 2\nname: node-scope\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: ./roles/observer.yaml\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      command: [the provider below]\n",
            "    mode: bypass\n",
        ),
        &[
            (FAKE_HARNESS_KEY, fake_harness()),
            ("members.worker.judge.command.0", fake_provider()),
        ],
    ));
    // A base with no supervisor of its own, and a persona that is one field.
    workspace.write("base.yaml", "provider:\n  kind: oneharness\n");
    workspace.write(
        "roles/observer.yaml",
        "system_prompt: |\n  Observer marker: judge the surface, do not restate it.\n",
    );

    let record = workspace.at("prompts.txt");
    let run = workspace.run_task(&format!(
        "fake:complete-now: role only fake:record-prompt={}",
        record.display()
    ));
    run.expect_code(0);
    assert_eq!(
        run.of_kind("member-settled")[0]["payload"]["completed"],
        serde_json::json!(true)
    );
    assert!(
        std::fs::read_to_string(&record)
            .expect("prompts")
            .contains("Observer marker: judge the surface, do not restate it."),
        "the role never reached the agent"
    );

    // And no supervisor was invented for it: to onejudge an empty `user:` is a
    // simulated user with an empty persona, not the absent one the base asked
    // for.
    let effective = workspace.member_file("worker", "onejudge.yaml");
    let config: serde_json::Value =
        serde_norway::from_str(&effective).expect("the effective config is YAML");
    assert!(
        config.get("user").is_none(),
        "a simulated user was invented: {effective}"
    );
    assert_eq!(
        config["system_prompt"].as_str(),
        Some("Observer marker: judge the surface, do not restate it."),
        "{effective}"
    );
}

/// A base config the merge cannot read is refused, with what was found.
#[test]
fn a_base_config_of_the_wrong_shape_refuses_the_run() {
    let workspace = Workspace::new();
    // A bar of the wrong shape is refused with what was found, rather than read
    // as a base that set none — which would run the dispatch under whatever the
    // persona brought and no shared review bar at all.
    workspace.write(
        "base.yaml",
        &BASE.replace("done_when: \"the task is complete\"", "done_when: [a, b]"),
    );
    let wrong_shape = workspace.run_task("fake:complete-now: a bar of the wrong shape");
    wrong_shape.expect_code(2);
    assert!(
        wrong_shape
            .stderr
            .contains("`user.done_when` must be a string, got a list"),
        "{}",
        wrong_shape.stderr
    );
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
    workspace
        .graph(&two_party_graph(&fake_harness(), NO_ENV).replace("    persona: engineer\n", ""));
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

    workspace
        .graph(&two_party_graph(&fake_harness(), NO_ENV).replace("    persona: engineer\n", ""));

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

    workspace.graph(&two_party_graph(&fake_harness(), NO_ENV).replace("    mode: bypass\n", ""));
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
    workspace.graph(&graph_with(
        concat!(
            "version: 2\nname: node-scope\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    max_turns: 4\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n      stream: true\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
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
    workspace.graph(&graph_with(
        concat!(
            "version: 1\nname: node-scope\n",
            "env: {}\n",
            "members:\n",
            "  build:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  report:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    deps: [build]\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
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

/// An unsuccessful dependency blocks the whole chain behind it. The skipped
/// members never publish `member-started`; their distinct outcomes are carried
/// by `graph-settled` and the durable run record.
#[test]
fn a_failed_dependency_skips_and_propagates_through_its_chain() {
    let workspace = Workspace::new();
    // The doubled harness is wired in even though the failing member never
    // produces an answer: the two members behind it are skipped rather than run,
    // and a graph that reached a paid harness to prove a skip would be paying
    // for the part of the journey that is not the point.
    workspace.graph(&graph_with(
        concat!(
            "version: 2\nname: node-scope\n",
            "env: {}\n",
            "members:\n",
            "  build:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  report:\n    kind: onejudge\n    base_config: ./base.yaml\n",
            "    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n    deps: [build]\n",
            "  publish:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "    deps: [report]\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness().as_str())],
    ));
    // The first member fails because its **harness** does, which is the only way
    // left to fail one: its turn is a library call, so there is no `oneharness`
    // process to withhold. `FAKE_HARNESS_CRASH` is that failure — the harness
    // exits having published nothing, and the chain has no other candidate.
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: blocked",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[("FAKE_HARNESS_CRASH", "1")],
    );
    run.expect_code(1);
    let started: Vec<String> = run
        .of_kind("member-started")
        .iter()
        .filter_map(|event| labels(event).get("member").cloned())
        .collect();
    assert_eq!(started, ["build"]);
    let settled = &run.of_kind("graph-settled")[0]["payload"]["members"];
    assert_eq!(settled["report"], "skipped (build)");
    assert_eq!(settled["publish"], "skipped (report)");
    let record = workspace.record();
    assert_eq!(record["members"]["report"], "skipped (build)");
    assert_eq!(record["members"]["publish"], "skipped (report)");
}

#[test]
fn a_two_party_member_can_depend_on_a_worker() {
    let workspace = Workspace::new();
    workspace.graph(&graph_with(
        concat!(
            "version: 2\nname: node-scope\n",
            "env: {}\n",
            "members:\n",
            "  build:\n    kind: oneharness\n    oneharness_config: ./oneharness.toml\n",
            "  supervisor:\n    kind: onejudge\n    base_config: ./base.yaml\n",
            "    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n    deps: [build]\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
    ));
    let run = workspace.run_task("fake:complete-now: supervised after build");
    run.expect_code(0);
    let events = run.events();
    let at = |member: &str, kind: &str| {
        events
            .iter()
            .position(|event| {
                event["kind"] == kind && labels(event).get("member").is_some_and(|m| m == member)
            })
            .unwrap_or_else(|| panic!("no {kind} for {member}"))
    };
    assert!(at("build", "member-settled") < at("supervisor", "member-started"));
}

#[test]
fn a_two_party_member_refuses_missing_and_cyclic_dependencies() {
    let workspace = Workspace::new();
    let graph = graph_with(
        concat!(
            "version: 2\nname: node-scope\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: onejudge\n    base_config: ./base.yaml\n",
            "    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n    deps: [DEPENDENCY]\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
    );
    workspace.graph(&graph.replace("DEPENDENCY", "ghost"));
    let missing = workspace.run(&["validate", "./graph.yaml"]);
    missing.expect_code(2);
    assert!(missing.stderr.contains("ghost"), "{}", missing.stderr);

    workspace.graph(&graph.replace("DEPENDENCY", "worker"));
    let cyclic = workspace.run(&["validate", "./graph.yaml"]);
    cyclic.expect_code(2);
    assert!(cyclic.stderr.contains("cycle"), "{}", cyclic.stderr);
}

/// A run under a base config the member cannot even read refuses with the path,
/// not a parse trace.
#[test]
fn an_unreadable_ref_refuses_with_the_path_it_could_not_read() {
    let workspace = Workspace::new();
    workspace
        .graph(&two_party_graph(&fake_harness(), NO_ENV).replace("./base.yaml", "./nowhere.yaml"));
    let run = workspace.run_task("fake:complete-now: unreadable");
    run.expect_code(2);
    assert!(run.stderr.contains("nowhere.yaml"), "{}", run.stderr);
}

/// **No `oneharness` process is spawned for a single-sided member's turn.**
///
/// Proven by taking the binary away. `ONEAGENTGRAPH_ONEHARNESS_BIN` is the name
/// this crate spawns `oneharness` under, and pointing it at something that is
/// not installed used to kill a single-sided member outright — the turn *was*
/// that process. The member settles now, because its turn is
/// `oneharness_core::io::run::run_supervised` called here, and nothing on the
/// turn path looks the binary up at all.
///
/// This is the acceptance criterion for the conversion, driven the only way that
/// cannot pass by accident: an assertion that the member ran is worthless unless
/// a spawn would have failed, and here one would have. The harness underneath is
/// still a child process — it is just oneharness's to start, not this crate's.
#[test]
fn a_single_sided_members_turn_spawns_no_oneharness_process() {
    let workspace = Workspace::new();
    workspace.graph(&single_sided_graph(&fake_harness()));
    let run = workspace.run_with(
        &[
            "run",
            "./graph.yaml",
            "--task",
            "fake:complete-now: no binary needed",
            "--dir",
            &workspace.dir().display().to_string(),
        ],
        &[(
            "ONEAGENTGRAPH_ONEHARNESS_BIN",
            "oneharness-that-is-not-installed",
        )],
    );
    run.expect_code(0);
    assert!(
        run.of_kind("member-died").is_empty(),
        "a member died without an `oneharness` binary, so its turn still needed \
         one: {:?}",
        run.kinds()
    );
    let started = run.of_kind("member-started");
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(
        started[0]["payload"]["engine"],
        serde_json::json!("oneharness")
    );
    // And it really took its turn, through the doubled harness oneharness itself
    // spawned — a member that settled having run nothing would prove nothing.
    assert_eq!(
        stored_report(&run, "reporter")["results"][0]["text"],
        serde_json::json!("done"),
    );
}

/// A single-sided member whose run oneharness **refuses** dies as `unstartable`,
/// with the stream saying so — rather than the run reporting a settled graph.
///
/// A refusal is the request being unhonourable, decided before anything is
/// spawned, and it is what replaces a `Command::spawn` that failed: the config
/// here names a harness id that does not exist, which oneharness rejects when it
/// loads the file. What this proves about the payload is the half the conversion
/// settled: a member that never started has no exit code, no disposition, and no
/// standard error, so it says why through `cause` and `detail` instead of
/// reporting a process's facts as null.
#[test]
fn a_member_whose_run_is_refused_is_a_death_the_stream_names() {
    let workspace = Workspace::new();
    workspace.write(
        "oneharness.toml",
        "run_mode = \"fallback\"\nharnesses = [\"not-a-real-harness\"]\n",
    );
    workspace.graph(&single_sided_graph(&fake_harness()));
    let run = workspace.run_task("fake:complete-now: refused");
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
            .contains("not-a-real-harness"),
        "the refusal did not name what it refused: {died:?}"
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
    workspace.graph(&graph_with(
        concat!(
            "version: 1\nname: node-scope\n",
            "env: {}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
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

/// The report one member stored, read from the path its settle named.
fn stored_report(run: &crate::support::Run, member: &str) -> serde_json::Value {
    let settled = run
        .of_kind("member-settled")
        .into_iter()
        .find(|event| {
            labels(event)
                .get("member")
                .is_some_and(|name| name.as_str() == member)
        })
        .unwrap_or_else(|| panic!("{member} never settled:\n{}", run.stdout));
    let path = settled["payload"]["report_path"]
        .as_str()
        .unwrap_or_else(|| panic!("{member}'s settle named no stored report: {settled}"));
    assert_eq!(
        settled["payload"]["completed"],
        serde_json::json!(true),
        "{member} did not settle successfully: {settled}"
    );
    let raw = std::fs::read_to_string(path).expect("the stored report");
    serde_json::from_str(&raw).expect("the stored report is JSON")
}

/// A member whose own oneharness config asks for structured output gets it: the
/// run does not stream, the answer is validated against the schema that config
/// names, and the settle stores the document carrying it.
///
/// Two overrides this crate used to make are what the journey is really about,
/// and both are load-bearing here:
///
/// * The forced `--stream`. A flag beats config in oneharness and is mutually
///   exclusive with a schema there, so a member declaring `schema_file` was a
///   usage error rather than a structured-output run. The `reporter` beside the
///   drafter is the other half of that: a config declaring neither still streams,
///   argv for argv, which is what every graph already written depends on.
/// * The relative path. The drafter's config lives **outside** the directory the
///   member works in and names its schema relative to itself, which is where the
///   operator wrote it. oneharness resolves a config-declared `schema_file`
///   against `--cwd`, so without anchoring it looks for the schema under the work
///   directory, finds nothing, and the member dies before a turn.
#[test]
fn a_single_sided_member_answers_with_the_structured_document_its_config_asks_for() {
    let workspace = Workspace::new();
    // Beside the config that names it, one directory away from where the member
    // works — the arrangement an operator keeps their graph's configs in.
    workspace.write(
        "configs/oneharness.structured.toml",
        &format!(
            "{CHAIN}schema_file = \"./answer.schema.json\"\nhistory = true\nhistory_dir = \"./history\"\n"
        ),
    );
    workspace.write(
        "configs/answer.schema.json",
        concat!(
            "{\"type\": \"object\", \"required\": [\"title\", \"body\"],",
            " \"properties\": {\"title\": {\"type\": \"string\"},",
            " \"body\": {\"type\": \"string\"}},",
            " \"additionalProperties\": false}\n"
        ),
    );
    let answer = workspace.write(
        "answer.json",
        "{\"title\": \"take the config as written\", \"body\": \"one change request\"}",
    );

    workspace.graph(&graph_with(
        concat!(
            "version: 3\nname: node-scope\n",
            "env: {}\n",
            "members:\n  drafter:\n    kind: oneharness\n",
            "    oneharness_config: ./configs/oneharness.structured.toml\n",
            "  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
            "    task: \"fake:complete-now: say what changed\"\n",
        ),
        &[
            (FAKE_HARNESS_KEY.to_string(), fake_harness()),
            (
                "members.drafter.task".to_string(),
                format!("draft the body. fake:answer-file={}", answer.display()),
            ),
        ],
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    // The member the operator asked for a validated answer from is not streamed,
    // because there is no such run one layer down. Asserted where it shows —
    // the `turn-activity` a streaming member publishes and this one does not,
    // below — rather than on an argv there no longer is.
    //
    // And the answer came back validated, in the report the settle stored.
    let report = stored_report(&run, "drafter");
    let result = &report["results"][0];
    assert_eq!(result["schema_valid"], serde_json::json!(true), "{result}");
    assert_eq!(
        result["structured"],
        serde_json::json!({"title": "take the config as written", "body": "one change request"}),
        "the stored report carries no structured answer: {result}"
    );

    // Every path in that config was anchored, not just the one the schema run
    // needed: the history this member was told to keep landed beside the config
    // that asked for it, which is the only directory its author named.
    assert!(
        workspace.at("configs/history").is_dir(),
        "a relative `history_dir` was resolved against a directory nobody named"
    );
    assert!(
        !workspace.dir().join("history").exists(),
        "the history landed in the directory the member works in"
    );

    // The member beside it declares neither a schema nor `stream`, and runs
    // exactly as it did before: the tool events only a streamed member
    // publishes.
    let activity: Vec<String> = run
        .of_kind("turn-activity")
        .iter()
        .filter_map(|event| labels(event).get("member").cloned())
        .collect();
    assert_eq!(
        activity
            .iter()
            .filter(|member| member.as_str() == "reporter")
            .count(),
        1,
        "the streaming member published no tool event: {activity:?}"
    );
    assert!(
        !activity.iter().any(|member| member.as_str() == "drafter"),
        "a member that does not stream published a streamed event: {activity:?}"
    );
}

/// A member whose config says `stream = false` is honoured rather than
/// overridden: it publishes one report at the end and settles on it.
///
/// The buffered report is not merely "the same run without events". `oneharness
/// run` pretty-prints it, and this crate reads a member's stdout a line at a
/// time — so a member that stopped streaming and lost its report would settle as
/// a provider failure, which is exactly what this asserts it does not do.
#[test]
fn a_single_sided_member_that_asks_not_to_stream_still_settles_on_its_report() {
    let workspace = Workspace::new();
    workspace.write("oneharness.toml", &format!("{CHAIN}stream = false\n"));
    workspace.graph(&single_sided_graph(&fake_harness()));
    let run = workspace.run_task("fake:complete-now: one report at the end");
    run.expect_code(0);

    assert!(
        run.of_kind("turn-activity").is_empty(),
        "`stream = false` was overridden: a member that does not stream published \
         streamed events: {:?}",
        run.kinds()
    );
    let report = stored_report(&run, "reporter");
    assert_eq!(
        report["results"][0]["text"],
        serde_json::json!("done"),
        "the buffered report never reached the settle: {report}"
    );

    // And saying it beside a schema is the same run, not a contradiction: the
    // pair a config may legally declare — the operator asking for the buffered
    // report the schema already implies — runs and answers validated JSON.
    workspace.write(
        "answer.schema.json",
        concat!(
            "{\"type\": \"object\", \"required\": [\"title\"],",
            " \"properties\": {\"title\": {\"type\": \"string\"}},",
            " \"additionalProperties\": false}\n"
        ),
    );
    let answer = workspace.write("answer.json", "{\"title\": \"said out loud\"}");
    workspace.write(
        "oneharness.toml",
        &format!("{CHAIN}stream = false\nschema_file = \"./answer.schema.json\"\n"),
    );
    let both = workspace.run_task(&format!("draft it. fake:answer-file={}", answer.display()));
    both.expect_code(0);
    assert!(
        both.of_kind("turn-activity").is_empty(),
        "the pair a config may legally declare started streaming: {:?}",
        both.kinds()
    );
    assert_eq!(
        stored_report(&both, "reporter")["results"][0]["structured"],
        serde_json::json!({"title": "said out loud"}),
    );
}

/// A relative `env_file` in a member's config is anchored the same way, and the
/// refusal proves where it pointed: oneharness reads that file to assemble the
/// identity's environment, so the path it names is the path it looked in.
///
/// Deliberately the *missing* file. oneharness resolves a variant's environment
/// before it spawns anything, so this journey reaches the resolution and stops.
///
/// The candidate still has to be **installed** to get that far — a chain whose
/// only candidate has no binary fails as `not-installed` having resolved
/// nothing, which is what this asserted against on a host that happened to have
/// the paid CLI and nowhere else. A variant is not reachable by the
/// `ONEHARNESS_BIN_<ID>` override every other journey uses (that keys on a
/// harness id, and there is no spelling of it that carries a variant), so this
/// one names its provider where a variant can: oneharness's own deterministic
/// responder, `mock-harness`, which ships inside the `oneharness` this suite
/// already drives and is selected by the variable its own `run --mock-harness`
/// sets. Nothing paid is reachable from this config, on any machine, and no
/// journey turns on a harness being installed.
#[test]
fn a_relative_env_file_in_a_members_config_is_read_beside_that_config() {
    let workspace = Workspace::new();
    workspace.write(
        "configs/oneharness.identity.toml",
        &format!(
            concat!(
                "run_mode = \"fallback\"\nharnesses = [\"claude-code:alternate\"]\n",
                "\n[harness.claude-code.variant.alternate]\n",
                "bin = {bin}\nenv_file = \"./identity.env\"\n",
                "\n[harness.claude-code.variant.alternate.env]\n",
                "ONEHARNESS_INTERNAL_MOCK_HARNESS = \"1\"\n",
            ),
            // Quoted as TOML quotes it, so a Windows path's backslashes are not
            // read as escapes by the file this writes.
            bin = toml_edit::Value::from(oneharness_bin()),
        ),
    );
    workspace.graph(&graph_with(
        concat!(
            "version: 1\nname: node-scope\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: oneharness\n",
            "    oneharness_config: ./configs/oneharness.identity.toml\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        "fake:complete-now: never gets an identity",
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(1);

    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{died:?}");
    let detail = died[0]["payload"]["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains(
            &workspace
                .at("configs")
                .join("identity.env")
                .display()
                .to_string()
        ),
        "the identity file was looked for somewhere its author never named: {detail}"
    );
    assert!(
        !detail.contains(&workspace.dir().display().to_string()),
        "the identity file was resolved against the directory the member works in: {detail}"
    );
}

/// An answer that does not satisfy the schema is a member that **failed**, not
/// one that quietly answered something else.
///
/// The other half of the structured-output journey above, and the half a
/// consumer depends on: `onepipeline` reads the stored report for a validated
/// document, so a run that could not produce one has to say so. oneharness
/// re-prompts and then reports the harness as failed, which reaches the stream
/// as a death with the exit code the run really made — never a `member-settled`
/// carrying an unvalidated answer.
#[test]
fn a_structured_answer_that_fails_its_schema_fails_the_member() {
    let workspace = Workspace::new();
    workspace.write(
        "configs/oneharness.structured.toml",
        &format!("{CHAIN}schema_file = \"./answer.schema.json\"\nschema_max_retries = 0\n"),
    );
    workspace.write(
        "configs/answer.schema.json",
        concat!(
            "{\"type\": \"object\", \"required\": [\"title\"],",
            " \"properties\": {\"title\": {\"type\": \"string\"}},",
            " \"additionalProperties\": false}\n"
        ),
    );
    // Valid JSON, and not the document that was asked for.
    let answer = workspace.write("answer.json", "{\"title\": 5, \"extra\": true}");

    workspace.graph(&graph_with(
        concat!(
            "version: 1\nname: node-scope\n",
            "env: {}\n",
            "members:\n  drafter:\n    kind: oneharness\n",
            "    oneharness_config: ./configs/oneharness.structured.toml\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &format!("draft the body. fake:answer-file={}", answer.display()),
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(1);

    assert!(
        run.of_kind("member-settled").is_empty(),
        "a member that never produced a validated answer settled anyway: {:?}",
        run.kinds()
    );
    let died = run.of_kind("member-died");
    assert_eq!(died.len(), 1, "{died:?}");
    assert_eq!(labels(&died[0])["member"], "drafter");
    assert_eq!(
        died[0]["payload"]["rule"],
        serde_json::json!("provider-failure")
    );
}

/// A two-party member's sides are the same operator's configs, anchored the same
/// way: the agent side's own `history_dir` is written beside the config that
/// named it.
///
/// Asserted for the *other* member kind because the anchoring is one rule for
/// both — each side's config is resolved and stamped into the member's scratch
/// by the same call — and a rule proven on only one of them is one that can
/// silently stop holding for the other.
#[test]
fn a_two_party_members_side_config_keeps_its_paths_where_they_were_written() {
    let workspace = Workspace::new();
    workspace.write(
        "configs/oneharness.agent.toml",
        &format!("{CHAIN}history = true\nhistory_dir = \"./history\"\n"),
    );
    workspace.graph(&graph_with(
        concat!(
            "version: 1\nname: node-scope\n",
            "env: {}\n",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./configs/oneharness.agent.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
    ));
    let run = workspace.run_task("fake:complete-now: write the thing");
    run.expect_code(0);

    assert!(
        workspace.at("configs/history").is_dir(),
        "the agent side's relative `history_dir` was resolved against a directory \
         nobody named: {:?}",
        std::fs::read_dir(workspace.path())
            .expect("the workspace")
            .flatten()
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>()
    );
}

/// A path that already means one thing is carried through untouched: an absolute
/// one, and an empty value oneharness reads as unset.
///
/// The other side of anchoring, and the half that would break silently. An
/// absolute `schema_file` joined onto the config's directory would name a file
/// nobody wrote, and an empty `history_dir` joined onto it would turn a key that
/// said nothing into one naming that directory — so this asserts the run
/// answered on the schema its author named *and* that the history landed in
/// oneharness's own default store rather than beside the config.
#[test]
fn a_config_whose_paths_are_already_unambiguous_is_carried_through_as_written() {
    let workspace = Workspace::new();
    let schema = workspace.write(
        "schemas/answer.schema.json",
        concat!(
            "{\"type\": \"object\", \"required\": [\"title\"],",
            " \"properties\": {\"title\": {\"type\": \"string\"}},",
            " \"additionalProperties\": false}\n"
        ),
    );
    workspace.write(
        "configs/oneharness.absolute.toml",
        &format!(
            "{CHAIN}schema_file = {}\nhistory = true\nhistory_dir = \"\"\n",
            // Quoted as TOML quotes it, so a Windows path's backslashes are not
            // read as escapes by the file this writes.
            toml_edit::Value::from(schema.display().to_string())
        ),
    );
    let answer = workspace.write("answer.json", "{\"title\": \"already unambiguous\"}");

    workspace.graph(&graph_with(
        concat!(
            "version: 1\nname: node-scope\n",
            "env: {}\n",
            "members:\n  drafter:\n    kind: oneharness\n",
            "    oneharness_config: ./configs/oneharness.absolute.toml\n",
        ),
        &[(FAKE_HARNESS_KEY, fake_harness())],
    ));
    let run = workspace.run(&[
        "run",
        "./graph.yaml",
        "--task",
        &format!("draft it. fake:answer-file={}", answer.display()),
        "--dir",
        &workspace.dir().display().to_string(),
    ]);
    run.expect_code(0);

    // The absolute schema was read where it was written, so the answer validated.
    let report = stored_report(&run, "drafter");
    assert_eq!(
        report["results"][0]["structured"],
        serde_json::json!({"title": "already unambiguous"}),
        "{report}"
    );
    // And the empty history directory stayed unset, so oneharness resolved its
    // own default store for it: nothing was written beside the config, which is
    // the one place anchoring an empty value could have put it. Where that
    // default *is* is oneharness's own question and differs per platform, so it
    // is deliberately not asserted here.
    assert!(
        !workspace.at("configs/history").exists(),
        "an empty `history_dir` was turned into the config's own directory"
    );
}
