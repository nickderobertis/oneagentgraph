//! The YAML this crate meets for real, read and written as it was before the
//! reader moved to serde-saphyr.
//!
//! `tests/yaml-corpus/README.md` says where every file came from. Three things
//! are held here:
//!
//! * every committed graph — the `graph.v*.yaml` goldens and the screenshot
//!   fixtures — and the host's real graphs read, through the graph loader, to the
//!   `GraphConfig` the crate read before the move, as `expected.json` records it;
//! * every shipped persona and the host's real ones read, through the persona
//!   loader and the real merge over the host's base config, to the label and
//!   the merged config recorded there; and the host's base config, which carries
//!   more consecutive comments than serde-saphyr's reader buffers, reads at all;
//! * the effective `onejudge.yaml` each real two-party member writes — every one
//!   in the host's graphs, and the node-scope worker under each host persona —
//!   reads back through the linked onejudge to the `Config` the text written
//!   before the move reads to.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf, MAIN_SEPARATOR};

use oneagentgraph::config::{ConfigRef, GraphConfig, Member, TaskText};
use oneagentgraph::persona::{self, Persona};
use oneagentgraph::resolve::Resolver;
use serde_json::Value;

const HOST_BASE: &str = "tests/yaml-corpus/host/config/onejudge.base.yaml";
const SCRATCH: &str = "__SCRATCH__";
const HOST: &str = "__HOST__";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|err| panic!("{path}: {err}"))
}

fn expected() -> Value {
    serde_json::from_str(&read("tests/yaml-corpus/expected.json")).expect("expected.json is JSON")
}

/// The `.yaml` files directly under `dir`, as paths from the crate root.
fn yaml_in(dir: &str) -> BTreeSet<String> {
    std::fs::read_dir(root().join(dir))
        .unwrap_or_else(|err| panic!("{dir}: {err}"))
        .map(|entry| entry.expect("a directory entry").file_name())
        .filter_map(|name| {
            let name = name.to_str()?.to_string();
            name.ends_with(".yaml").then(|| format!("{dir}/{name}"))
        })
        .collect()
}

fn recorded<'a>(expected: &'a Value, section: &str) -> &'a serde_json::Map<String, Value> {
    expected[section]
        .as_object()
        .unwrap_or_else(|| panic!("expected.json has no `{section}`"))
}

/// The graph loader, with nothing applied between the parse and the read.
fn load_graph(path: &str) -> Result<GraphConfig, oneagentgraph::error::Error> {
    oneagentgraph::run::read_graph(&read(path), path, |_| Ok(()))
}

#[test]
fn every_committed_and_host_graph_reads_to_the_graph_it_read_before() {
    let expected = expected();
    let graphs = recorded(&expected, "graphs");
    // Every graph golden and every host graph is recorded, so one added later
    // is not silently left out of this.
    let mut present: BTreeSet<String> = yaml_in("tests/golden");
    present.extend(yaml_in("tests/yaml-corpus/host/graphs"));
    for path in &present {
        assert!(graphs.contains_key(path), "{path} has no recorded reading");
    }
    for (path, outcome) in graphs {
        match (load_graph(path), outcome.get("read")) {
            (Ok(graph), Some(before)) => {
                let before_graph: GraphConfig = serde_json::from_value(before.clone())
                    .unwrap_or_else(|err| panic!("{path}: the recorded graph reads back: {err}"));
                assert_eq!(graph, before_graph, "{path} reads to another graph");
                assert_eq!(
                    &serde_json::to_value(&graph).expect("a graph is JSON"),
                    before,
                    "{path} reads to another graph"
                );
            }
            (Err(err), None) => assert!(err.to_string().contains(path.as_str()), "{err}"),
            (now, _) => panic!("{path}: read before as {outcome}, now as {now:?}"),
        }
    }
}

#[test]
fn every_shipped_and_host_persona_reads_and_merges_to_what_it_did_before() {
    let expected = expected();
    let personas = recorded(&expected, "personas");
    let mut present = yaml_in("personas");
    present.extend(yaml_in("tests/yaml-corpus/host/personas"));
    present.extend(yaml_in("tests/yaml-corpus/host/personas/crozier"));
    for path in &present {
        assert!(
            personas.contains_key(path),
            "{path} has no recorded reading"
        );
    }
    let base = read(HOST_BASE);
    for (path, outcome) in personas {
        match Persona::parse(&read(path), path) {
            Ok(persona) => {
                assert_eq!(
                    persona.label(),
                    outcome["label"].as_str(),
                    "{path} carries another label"
                );
                let merged = persona::merge(&base, HOST_BASE, &persona)
                    .unwrap_or_else(|err| panic!("{path} over the host's base: {err}"));
                assert_eq!(
                    &merged, &outcome["merged"],
                    "{path} merges to another config"
                );
            }
            Err(err) => assert!(
                outcome.get("refused").is_some(),
                "{path} read before, and is now refused: {err}"
            ),
        }
    }
}

/// The host's base config is the `base_config` of every real two-party graph,
/// and it carries a run of comments between `user:` and its first entry longer
/// than serde-saphyr buffers when comments are kept.
#[test]
fn the_host_base_config_reads_as_a_members_base_through_the_real_merge() {
    let expected = expected();
    for (path, before) in recorded(&expected, "bases") {
        let merged = persona::merge(&read(path), path, &Persona::default())
            .unwrap_or_else(|err| panic!("{path} does not read as a base: {err}"));
        assert_eq!(&merged, before, "{path} merges to another config");
    }
}

/// One real two-party member: the graph it is in, its name, and the host
/// persona it runs under in place of its own, if any.
struct Case {
    file: String,
    graph: GraphConfig,
    name: String,
    member: Member,
}

fn host() -> PathBuf {
    root().join("tests/yaml-corpus/host")
}

/// Every two-party member of the host's graphs, and the node-scope worker under
/// each host persona — the override `onepipeline` forwards from a plan node.
fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for path in yaml_in("tests/yaml-corpus/host/graphs") {
        let graph = load_graph(&path).expect("a host graph reads");
        let stem = Path::new(&path)
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        for (name, member) in &graph.members {
            if let Member::Onejudge(_) = member {
                cases.push(Case {
                    file: format!("{stem}.{name}.yaml"),
                    graph: graph.clone(),
                    name: name.clone(),
                    member: member.clone(),
                });
            }
        }
    }
    let node_scope = load_graph("tests/yaml-corpus/host/graphs/node-scope.yaml").expect("reads");
    let mut personas = yaml_in("tests/yaml-corpus/host/personas");
    personas.extend(yaml_in("tests/yaml-corpus/host/personas/crozier"));
    for path in personas {
        let relative = path.trim_start_matches("tests/yaml-corpus/host/");
        let label = relative
            .trim_start_matches("personas/")
            .trim_end_matches(".yaml")
            .replace('/', "-");
        let mut member = node_scope.members["worker"].clone();
        let Member::Onejudge(worker) = &mut member else {
            panic!("the node-scope worker is a two-party member")
        };
        worker.persona = Some(ConfigRef(format!("../{relative}")));
        cases.push(Case {
            file: format!("node-scope.worker.persona-{label}.yaml"),
            graph: node_scope.clone(),
            name: "worker".to_string(),
            member,
        });
    }
    cases
}

fn onejudge_reads(text: &str, what: &str) -> String {
    let config = onejudge::cli::Config::from_yaml(text)
        .unwrap_or_else(|err| panic!("onejudge refuses {what}: {err}\n{text}"));
    format!("{config:#?}")
}

#[test]
fn every_real_two_party_members_config_reads_back_to_what_it_did_before() {
    let host = host();
    let scratch_root = tempfile::tempdir().expect("a scratch root");
    let recorded_dir = root().join("tests/yaml-corpus/effective");
    let mut compared = BTreeSet::new();
    for case in cases() {
        let scratch = scratch_root
            .path()
            .join(case.file.trim_end_matches(".yaml"));
        let graph_dir = host.join("graphs");
        let context = oneagentgraph::invoke::Context {
            dir: &host,
            scratch: &scratch,
            graph_dir: Some(&graph_dir),
            personas: case.graph.personas.as_deref(),
            task: Some("the task"),
            task_text: TaskText::under(case.graph.version),
            session: "capture",
            oneharness_bin: "oneharness",
            background: case.graph.is_background(&case.name),
        };
        let built = oneagentgraph::invoke::build(&case.member, &context, &mut Resolver::new());
        let before_path = recorded_dir.join(&case.file);
        if !before_path.exists() {
            // A persona the base refused as an override is refused still: the
            // shipped template's placeholder name is not a name.
            let err = built.expect_err(&format!("{} built, but wrote nothing before", case.file));
            assert!(
                err.to_string().contains("persona name"),
                "{}: {err}",
                case.file
            );
            continue;
        }
        built.unwrap_or_else(|err| panic!("{}: {err}", case.file));
        let written = std::fs::read_to_string(scratch.join("onejudge.yaml")).expect("written");
        // The crate joins onto these directories natively, so a recorded
        // `/` after one is the separator this platform writes there.
        let before = std::fs::read_to_string(&before_path)
            .expect("recorded")
            .replace(
                &format!("{SCRATCH}/{}/", case.file.trim_end_matches(".yaml")),
                &format!("{}{MAIN_SEPARATOR}", scratch.display()),
            )
            .replace(
                &format!("{HOST}/graphs/"),
                &format!("{}{MAIN_SEPARATOR}", graph_dir.display()),
            );
        assert!(
            !before.contains(SCRATCH) && !before.contains(HOST),
            "{}: a recorded path this test does not place",
            case.file
        );
        assert_eq!(
            onejudge_reads(&written, &format!("{} as written now", case.file)),
            onejudge_reads(&before, &format!("{} as written before", case.file)),
            "{} reads back to another onejudge config",
            case.file
        );
        compared.insert(case.file);
    }
    let recorded: BTreeSet<String> = std::fs::read_dir(&recorded_dir)
        .expect("the recorded configs")
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(
        compared, recorded,
        "a recorded config no case wrote, or the reverse"
    );
}
