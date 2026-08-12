//! The workspace every journey drives, and the assertions they share.
//!
//! A journey builds a real graph on disk, runs the real `oneagentgraph` binary
//! against it, and reads back the NDJSON it produced. The only thing standing in
//! for something real is the paid harness process, replaced at oneharness's own
//! `ONEHARNESS_BIN_<ID>` seam by the crate's own `test-doubles` stand-in — real
//! `onejudge` drives the conversation, now as a library inside the binary under
//! test, and real `oneharness` selects the identity in between.

// llmlint: ignore-file[e2e_not_mocked] the paid model turn is the one genuinely
// external thing a gate cannot run for free, and this is the seam it is replaced
// at: oneharness's own documented `ONEHARNESS_BIN_<ID>` binary override. Real
// `oneagentgraph` prepares the launch, the real `onejudge` engine it links
// against runs the loop, and real `oneharness` selects the identity, spawns the
// double, classifies its refusal, and falls through. Nothing between them is
// stubbed, and no journey may widen this to a second seam.

// Journeys are modules, and each reaches for a different part of this support
// surface; an unused-here helper is used two files over, so `dead_code` would
// fire on the shared vocabulary rather than on anything unused.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// One journey's directories and the graph inside them.
pub struct Workspace {
    root: tempfile::TempDir,
    /// oneharness's own per-user state for this journey — see
    /// [`Workspace::session_store`] for why it is not simply a directory inside
    /// [`root`](Self::root).
    session_store: tempfile::TempDir,
}

/// What one `oneagentgraph` invocation produced.
pub struct Run {
    /// The process exit code.
    pub code: i32,
    /// Everything it wrote to stdout.
    pub stdout: String,
    /// Everything it wrote to stderr.
    pub stderr: String,
}

impl Run {
    /// Every envelope on stdout, in the order it was written.
    ///
    /// Panics with the whole stream when a line is not an envelope, because a
    /// stream a consumer cannot parse is the failure — not something to skip
    /// past on the way to an assertion.
    pub fn events(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|err| panic!("stdout line is not an envelope ({err}): {line}"))
            })
            .collect()
    }

    /// Every envelope of one kind.
    pub fn of_kind(&self, kind: &str) -> Vec<Value> {
        self.events()
            .into_iter()
            .filter(|event| event["kind"] == kind)
            .collect()
    }

    /// Every kind the stream carried, in the order it was written.
    pub fn kinds(&self) -> Vec<String> {
        self.events()
            .iter()
            .filter_map(|event| event["kind"].as_str().map(str::to_string))
            .collect()
    }

    /// Assert an exit code, showing the whole run when it is not the one meant.
    pub fn expect_code(&self, code: i32) -> &Self {
        assert_eq!(
            self.code, code,
            "expected exit {code}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.stdout, self.stderr
        );
        self
    }
}

impl Workspace {
    /// A workspace whose graph has one two-party member called `worker`.
    ///
    /// Both sides run a one-identity `claude-code` chain, which is what makes the
    /// fake reachable: `ONEHARNESS_BIN_*` keys on a harness id, and there is no
    /// spelling of it that reaches a *variant* — a chain naming variants here
    /// would spawn the real paid provider with the double sitting unused beside
    /// it, which is a money hazard rather than a style point.
    pub fn new() -> Self {
        let workspace = Self {
            root: tempfile::tempdir().expect("a workspace"),
            session_store: session_store(),
        };
        std::fs::create_dir_all(workspace.dir()).expect("work dir");
        std::fs::create_dir_all(workspace.state()).expect("state dir");
        workspace.write("oneharness.toml", CHAIN);
        workspace.write("oneharness.judge.toml", CHAIN);
        workspace.write("base.yaml", BASE);
        workspace.write("graph.yaml", &two_party_graph(&fake_harness(), ""));
        workspace
    }

    /// The directory the graph and its configs live in.
    pub fn path(&self) -> &Path {
        self.root.path()
    }

    /// oneharness's session store for this journey, which is where the control
    /// socket a turn is interrupted through is bound.
    pub fn session_store(&self) -> &Path {
        self.session_store.path()
    }

    /// The directory members work in.
    pub fn dir(&self) -> PathBuf {
        self.root.path().join("work")
    }

    /// Where run state lives.
    pub fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }

    /// One path inside the workspace.
    pub fn at(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// Write one file into the workspace, creating what holds it.
    pub fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self.at(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(&path, content).expect("write");
        path
    }

    /// Read one file back.
    pub fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.at(name)).unwrap_or_default()
    }

    /// Replace the graph with `document`.
    pub fn graph(&self, document: &str) -> &Self {
        self.write("graph.yaml", document);
        self
    }

    /// Run the binary with these arguments, and this extra environment.
    pub fn run_with(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let output = self.command(args, env).output().expect("the binary runs");
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Start the binary and leave it running, for a journey that needs to
    /// observe a run *while* something else happens.
    pub fn spawn_with(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Child {
        self.command(args, env)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the binary starts")
    }

    /// Start the binary with an input pipe deliberately held open by the test.
    pub fn spawn_with_open_stdin(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> std::process::Child {
        self.command(args, env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the binary starts")
    }

    /// The binary, armed with this workspace's directories and environment.
    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_oneagentgraph"));
        command
            .args(args)
            .current_dir(self.path())
            .env("ONEAGENTGRAPH_STATE_DIR", self.state())
            .env("ONEAGENTGRAPH_ONEHARNESS_BIN", oneharness_bin())
            // oneharness's own per-user state — its session store, and the
            // `control/<session>.sock` an `interrupt` addresses inside it. Its
            // own directory so a journey drives its own store rather than the
            // host's, and named here so the run and the `interrupt` that reaches
            // it resolve the same one: `run --session-dir` has no config or
            // environment layer, so this variable is what moves both.
            .env("XDG_STATE_HOME", self.session_store.path())
            // A journey must never inherit an enclosing dispatch's selection:
            // this suite runs inside one, and a leaked value would put the run
            // on an identity — and a bill — nobody in this test chose.
            .env_remove("ONEHARNESS_HARNESSES")
            .env_remove("ONEHARNESS_MODEL")
            .env_remove("ONEHARNESS_MODELS")
            .env_remove("ONEHARNESS_MODE")
            .env_remove("ONEHARNESS_TIMEOUT");
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }

    /// Run the binary with these arguments.
    pub fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[])
    }

    /// Run the default graph against one task, in this workspace's own work
    /// directory.
    ///
    /// Under the contract's production liveness bounds: a journey that needs
    /// shorter ones is testing a watchdog, and passes them itself through
    /// [`run_with`](Workspace::run_with) and [`bounds`].
    pub fn run_task(&self, task: &str) -> Run {
        self.run(&[
            "run",
            "./graph.yaml",
            "--task",
            task,
            "--dir",
            &self.dir().display().to_string(),
        ])
    }

    /// The one run this workspace recorded.
    pub fn record(&self) -> Value {
        let mut runs: Vec<PathBuf> = std::fs::read_dir(self.state())
            .expect("state")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        runs.sort();
        let run = runs.last().expect("a recorded run");
        let raw = std::fs::read_to_string(run.join("record.json")).expect("a record");
        serde_json::from_str(&raw).expect("the record is JSON")
    }
}

/// A journey's own oneharness session store, kept **short** and outside the
/// workspace on purpose.
///
/// A controlled turn is addressed through a unix domain socket at
/// `<store>/oneharness/sessions/control/<session>.sock`, and a unix socket path
/// has a hard kernel bound near 104 bytes. The session handle at the end of it is
/// `<run id>-<member>-skill`, which is around fifty of those on its own, and a
/// workspace tempdir on macOS lives under `/var/folders/…/T/` — so a store inside
/// the workspace put the socket past the bound, oneharness refused `--control`
/// for a run it could not bind one for, and every parked-turn journey timed out
/// waiting for a turn that was never controllable. `/tmp` keeps the prefix to a
/// dozen bytes on both unix platforms; nothing else needs it, so the fallback is
/// the platform's own temp directory.
fn session_store() -> tempfile::TempDir {
    let root = Path::new("/tmp");
    let mut builder = tempfile::Builder::new();
    let builder = builder.prefix("oag");
    if root.is_dir() {
        builder.tempdir_in(root)
    } else {
        builder.tempdir()
    }
    .expect("a session store")
}

/// A one-identity `claude-code` fallback chain — the only shape that keeps the
/// `ONEHARNESS_BIN_CLAUDE_CODE` double reachable.
pub const CHAIN: &str = "run_mode = \"fallback\"\nharnesses = [\"claude-code\"]\n";

/// A two-identity chain whose first candidate is the one a journey makes refuse.
pub const FALLBACK_CHAIN: &str =
    "run_mode = \"fallback\"\nharnesses = [\"claude-code\", \"codex\"]\n";

/// A chain spanning two harness families, which the model pairing rule forbids.
pub const MIXED_CHAIN: &str =
    "run_mode = \"fallback\"\nharnesses = [\"claude-code:alternate\", \"codex\"]\n";

/// The onejudge base every member merges its persona onto.
pub const BASE: &str = concat!(
    "provider:\n  kind: oneharness\n",
    "agent:\n  instructions: |\n    Standing bar: verify before you claim done.\n",
    "user:\n  done_when: \"the task is complete\"\n  max_turns: 4\n",
);

/// The default graph: one two-party member, both sides on the doubled chain.
pub fn two_party_graph(fake: &str, extra_env: &str) -> String {
    format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n{extra}",
            "members:\n  worker:\n    kind: onejudge\n",
            "    base_config: ./base.yaml\n    persona: engineer\n",
            "    agent:\n      oneharness_config: ./oneharness.toml\n",
            "    judge:\n      oneharness_config: ./oneharness.judge.toml\n",
            "    mode: bypass\n",
        ),
        fake = fake,
        extra = extra_env,
    )
}

/// A graph with one single-sided member — the kind that is still a child process
/// of its own, and so the only one a spawn failure is reachable through.
pub fn single_sided_graph(fake: &str) -> String {
    format!(
        concat!(
            "version: 1\nname: node-scope\n",
            "env:\n  ONEHARNESS_BIN_CLAUDE_CODE: {fake}\n",
            "members:\n  reporter:\n    kind: oneharness\n",
            "    oneharness_config: ./oneharness.toml\n",
        ),
        fake = fake,
    )
}

/// The compiled paid-harness double.
pub fn fake_harness() -> String {
    env!("CARGO_BIN_EXE_oneagentgraph-fake-harness").to_string()
}

/// The compiled onejudge `CommandProvider` double.
pub fn fake_provider() -> String {
    env!("CARGO_BIN_EXE_oneagentgraph-fake-provider").to_string()
}

/// The real `oneharness` this suite drives.
///
/// Resolved from the environment so a host or CI can pin an install, and
/// asserted present rather than skipped: a journey that quietly did not run the
/// real CLI proves nothing, and a suite that reports itself green on that basis
/// is worse than one that fails.
///
/// There is no companion for `onejudge`: the binary under test links the real
/// onejudge engine, so there is nothing on `PATH` for that half of the chain to
/// be shadowed by — the version it runs is the one `Cargo.lock` pins.
///
/// Probed for the `interrupt` verb rather than for merely running, because that
/// is the newest contract these journeys depend on: the turn-control pair
/// (`run --control` / `interrupt`) is what `interrupt.rs` drives, and a CLI
/// without it answers `--version` perfectly well before failing one layer down
/// as a bare `expected exit 0`. Everything else they need, they reach through
/// oneharness's own long-standing `ONEHARNESS_BIN_<ID>` override. `just`'s
/// `oneharness-version` pin governs what provisioning *installs*; asserting it
/// here too would be a second copy of that number, free to drift from the one
/// that matters.
pub fn oneharness_bin() -> String {
    required(
        "ONEAGENTGRAPH_TEST_ONEHARNESS",
        "oneharness",
        "the `interrupt` verb these journeys drive",
        |program| {
            Command::new(program)
                .args(["interrupt", "--help"])
                .output()
                .is_ok_and(|output| output.status.success())
        },
    )
}

/// One required external CLI: the pin, else the first candidate that carries the
/// contract this suite drives.
///
/// Resolution selects on *capability*, not on the name alone, because the name
/// alone is what the failure this guards against already satisfies. An older CLI
/// earlier on `PATH` than the one `just bootstrap` installs runs fine and
/// answers `--version` — it just rejects the flags every journey depends on, so
/// a name-only check hands the suite a binary that fails one layer down as a
/// bare `expected exit 0`. `docs/onejudge-integration.md` in ai-orchestrator
/// records this same shadowing as a live-dispatch outage; here it is a diagnosis
/// the suite makes for you, naming the missing contract and how to get it.
fn required(variable: &str, program: &str, contract: &str, carries: fn(&str) -> bool) -> String {
    if let Ok(pinned) = std::env::var(variable) {
        assert!(
            carries(&pinned),
            "{variable} points at `{pinned}`, which does not carry {contract}. The e2e suite \
             drives the real CLI as a subprocess — point it at an install that does, or unset it \
             and run `just bootstrap`."
        );
        return pinned;
    }
    for candidate in [program.to_string(), cargo_bin(program)] {
        if carries(&candidate) {
            return candidate;
        }
    }
    panic!(
        "no `{program}` carrying {contract} was found on PATH or in the cargo bin directory. The \
         e2e suite drives the real CLI as a subprocess — run `just bootstrap`, or set {variable} \
         to an install that carries it. A `{program}` that merely runs is not enough: an older \
         one earlier on PATH shadows the pinned install `just bootstrap` writes."
    );
}

/// Where `just bootstrap` installs a pinned CLI, which `PATH` need not reach
/// first.
fn cargo_bin(program: &str) -> String {
    let home = std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cargo")
        });
    home.join("bin").join(program).display().to_string()
}

/// The environment a journey uses to shorten the liveness bounds it is testing.
pub fn bounds(heartbeat: &str, stall: &str) -> Vec<(&'static str, String)> {
    vec![
        ("ONEAGENTGRAPH_HEARTBEAT_TIMEOUT", heartbeat.to_string()),
        ("ONEAGENTGRAPH_STALL_TIMEOUT", stall.to_string()),
    ]
}

/// Turn owned pairs into the borrowed ones [`Workspace::run_with`] takes.
pub fn as_env<'a>(pairs: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    pairs
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect()
}

/// Every reserved label an envelope carries, for an assertion that names them.
pub fn labels(event: &Value) -> BTreeMap<String, String> {
    event["labels"]
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_string),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Wait for `condition`, or fail saying what never happened.
///
/// The deadline is generous on purpose. Every journey here spawns three real
/// CLIs, and the suite runs many of them at once, so a threshold near one run's
/// unloaded wall clock fails healthy journeys under exactly the load the suite
/// creates — the same mistake a heartbeat near its write cadence makes.
pub fn until(what: &str, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
    while std::time::Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}
