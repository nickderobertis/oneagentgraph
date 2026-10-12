//! Differential harness: serde_norway 0.9 (what oneagentgraph uses today) against
//! serde-saphyr 1.3 (and saphyr 0.2's node tree for schema-less walks), over this
//! repository's real YAML and the real YAML of the host's ai-orchestrator
//! checkout. Throwaway: it prints what differs and decides nothing.
//!
//! Run: `spikes/saphyr-diff/run.sh` (captures the e2e suite's YAML first), or
//! `cargo run --release --manifest-path spikes/saphyr-diff/Cargo.toml -- \
//!    [--orchestrator DIR] [--e2e-capture DIR]`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use oneagentgraph::config::{GraphConfig, OneharnessMember, OnejudgeMember, TaskText};
use oneagentgraph::event::EventFilter;
use saphyr::LoadableYamlNode as _;
use serde_json::Value;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, Options, SerializerOptions};

// ---------------------------------------------------------------- reporting

#[derive(Default)]
struct Section {
    title: String,
    same: usize,
    diffs: Vec<String>,
    notes: Vec<String>,
    tally: BTreeMap<String, usize>,
}

impl Section {
    fn new(title: &str) -> Self {
        Section { title: title.to_string(), ..Default::default() }
    }
    fn same(&mut self) {
        self.same += 1;
    }
    fn diff(&mut self, text: String) {
        self.diffs.push(text);
    }
    fn note(&mut self, text: String) {
        self.notes.push(text);
    }
    fn print(&self) {
        println!("\n{}\n## {}\n", "=".repeat(78), self.title);
        println!(
            "cases identical: {}   cases differing: {}",
            self.same,
            self.diffs.len()
        );
        for note in &self.notes {
            println!("  note: {note}");
        }
        if !self.tally.is_empty() {
            println!("  classes of difference (count — class):");
            for (class, n) in &self.tally {
                println!("    {n:>4} — {class}");
            }
        }
        if self.diffs.is_empty() {
            println!("  NO DIFFERENCES for this kind.");
        }
        for (i, diff) in self.diffs.iter().enumerate() {
            println!("\n  --- difference {} ---", i + 1);
            for line in diff.lines() {
                println!("  {line}");
            }
        }
    }
}

fn json(value: &Value) -> String {
    serde_json::to_string(value).expect("json")
}

fn indent(text: &str) -> String {
    text.lines().map(|l| format!("    | {l}\n")).collect()
}

/// A small LCS line diff, printed as `-` (serde_norway) / `+` (serde-saphyr).
fn line_diff(before: &str, after: &str, limit: usize) -> String {
    let a: Vec<&str> = before.split_inclusive('\n').collect();
    let b: Vec<&str> = after.split_inclusive('\n').collect();
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = String::new();
    let mut shown = 0;
    let mut changed = 0;
    let show = |out: &mut String, tag: char, line: &str, shown: &mut usize| {
        if *shown < limit {
            let _ = writeln!(out, "{tag} {:?}", line.strip_suffix('\n').unwrap_or(line));
        }
        *shown += 1;
    };
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            show(&mut out, '+', b[j], &mut shown);
            changed += 1;
            j += 1;
        } else {
            show(&mut out, '-', a[i], &mut shown);
            changed += 1;
            i += 1;
        }
    }
    if shown > limit {
        let _ = writeln!(out, "  … {} more changed lines", shown - limit);
    }
    let _ = writeln!(out, "  ({changed} changed lines; '-' serde_norway, '+' serde-saphyr)");
    out
}

// ------------------------------------------------------------- the readers

fn de_default() -> Options {
    Options::default()
}

fn de_profiles() -> Vec<(&'static str, Options)> {
    let mut plain = Options::default();
    plain.with_snippet = false;
    let mut strict = Options::default();
    strict.strict_booleans = true;
    let mut ordinary = Options::default();
    ordinary.merge_keys = MergeKeyPolicy::AsOrdinary;
    let mut last = Options::default();
    last.duplicate_keys = DuplicateKeyPolicy::LastWins;
    let mut first = Options::default();
    first.duplicate_keys = DuplicateKeyPolicy::FirstWins;
    let mut octal = Options::default();
    octal.legacy_octal_numbers = true;
    let mut no_schema = Options::default();
    no_schema.no_schema = true;
    let mut finite = Options::default();
    finite.reject_non_finite_typeless_float = false;
    let mut quiet = Options::default();
    quiet.emit_comments = false;
    let mut roomy = Options::default();
    let mut budget = serde_saphyr::Budget::default();
    budget.max_buffered_comment_events = 1 << 20;
    roomy.budget = Some(budget);
    let combined = closest_options();
    vec![
        ("default", Options::default()),
        ("with_snippet=false", plain),
        ("strict_booleans=true", strict),
        ("merge_keys=AsOrdinary", ordinary),
        ("duplicate_keys=LastWins", last),
        ("duplicate_keys=FirstWins", first),
        ("legacy_octal_numbers=true", octal),
        ("no_schema=true", no_schema),
        ("reject_non_finite_typeless_float=false", finite),
        ("emit_comments=false", quiet),
        ("budget.max_buffered_comment_events=2^20", roomy),
        ("CLOSEST", combined),
    ]
}

/// The closest serde-saphyr reader configuration found: every option that
/// moves a measured difference toward today's behaviour, and none that moves
/// one away. Printed in full at the head of the report as `CLOSEST`.
fn closest_options() -> Options {
    let mut o = Options::default();
    o.strict_booleans = true;
    o.merge_keys = MergeKeyPolicy::AsOrdinary;
    o.duplicate_keys = DuplicateKeyPolicy::LastWins;
    o.with_snippet = false;
    o.emit_comments = false;
    o
}

fn ser_profiles() -> Vec<(&'static str, SerializerOptions)> {
    let mut wide = SerializerOptions::default();
    wide.compact_list_indent = false;
    let mut nulls = SerializerOptions::default();
    nulls.empty_as_braces = false;
    let mut flat = SerializerOptions::default();
    flat.prefer_block_scalars = false;
    let mut y12 = SerializerOptions::default();
    y12.yaml_12 = true;
    let mut flat_y12 = SerializerOptions::default();
    flat_y12.prefer_block_scalars = false;
    flat_y12.yaml_12 = true;
    let mut wrap = SerializerOptions::default();
    wrap.folded_wrap_chars = usize::MAX;
    wrap.min_fold_chars = usize::MAX;
    vec![
        ("default", SerializerOptions::default()),
        ("compact_list_indent=false", wide),
        ("empty_as_braces=false", nulls),
        ("prefer_block_scalars=false", flat),
        ("yaml_12=true", y12),
        ("prefer_block_scalars=false+yaml_12=true", flat_y12),
        ("folded_wrap_chars=MAX+min_fold_chars=MAX", wrap),
    ]
}

fn norway<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_norway::from_str(text).map_err(|e| e.to_string())
}

fn saphyr_with<T: serde::de::DeserializeOwned>(text: &str, options: Options) -> Result<T, String> {
    serde_saphyr::from_str_with_options(text, options).map_err(|e| e.to_string())
}

fn saphyr<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    saphyr_with(text, de_default())
}

fn saphyr_out<T: serde::Serialize>(value: &T, options: SerializerOptions) -> Result<String, String> {
    serde_saphyr::to_string_with_options(value, options).map_err(|e| e.to_string())
}

/// Today's production path for a graph (`run.rs`, `main.rs`): loose
/// `serde_json::Value`, then `to_value` / `from_value` into the typed graph.
fn graph_norway_production(text: &str) -> Result<GraphConfig, String> {
    let parsed: Value = serde_norway::from_str(text).map_err(|e| format!("parse: {e}"))?;
    serde_norway::from_value(serde_norway::to_value(&parsed).map_err(|e| format!("to_value: {e}"))?)
        .map_err(|e| format!("from_value: {e}"))
}

/// serde-saphyr has no `Value`/`to_value`/`from_value`. The two candidate
/// replacements for the typed half: `serde_json::from_value` straight off the
/// loose value, or a re-render through serde-saphyr and a typed re-read.
fn graph_saphyr_json(text: &str) -> Result<GraphConfig, String> {
    graph_saphyr_json_with(text, de_default())
}

fn graph_saphyr_json_with(text: &str, options: Options) -> Result<GraphConfig, String> {
    let parsed: Value = saphyr_with(text, options).map_err(|e| format!("parse: {e}"))?;
    serde_json::from_value(parsed).map_err(|e| format!("from_value: {e}"))
}

/// Which reader profiles make the production path (serde-saphyr loose read +
/// `serde_json::from_value`) agree with today's.
fn production_profiles(text: &str) -> Vec<&'static str> {
    let today = graph_norway_production(text);
    de_profiles()
        .into_iter()
        .filter(|(_, o)| match (&today, &graph_saphyr_json_with(text, o.clone())) {
            (Ok(x), Ok(y)) => x == y,
            (Err(_), Err(_)) => true,
            _ => false,
        })
        .map(|(n, _)| n)
        .collect()
}

fn graph_saphyr_rerender(text: &str) -> Result<GraphConfig, String> {
    let parsed: Value = saphyr(text).map_err(|e| format!("parse: {e}"))?;
    let rendered = saphyr_out(&parsed, SerializerOptions::default())?;
    saphyr(&rendered).map_err(|e| format!("reparse: {e}"))
}

/// saphyr 0.2's node tree, rendered so it can be set beside the
/// `serde_json::Value` serde_norway produces: what a `Value` cannot hold —
/// non-string keys, tags, aliases — is marked.
fn saphyr_tree(text: &str) -> Result<String, String> {
    let docs = saphyr::YamlOwned::load_from_str(text).map_err(|e| e.to_string())?;
    let mut out = String::new();
    match docs.as_slice() {
        [] => out.push_str("null"),
        [one] => render_tree(one, &mut out),
        many => {
            out.push_str("<MULTIDOC>");
            for d in many {
                render_tree(d, &mut out);
            }
        }
    }
    Ok(out)
}

fn render_tree(node: &saphyr::YamlOwned, out: &mut String) {
    use saphyr::{ScalarOwned, YamlOwned};
    match node {
        YamlOwned::Value(ScalarOwned::Null) => out.push_str("null"),
        YamlOwned::Value(ScalarOwned::Boolean(b)) => out.push_str(&b.to_string()),
        YamlOwned::Value(ScalarOwned::Integer(i)) => out.push_str(&i.to_string()),
        YamlOwned::Value(ScalarOwned::FloatingPoint(f)) => {
            out.push_str(&json(&serde_json::Number::from_f64(f.0).map_or(Value::Null, Value::Number)))
        }
        YamlOwned::Value(ScalarOwned::String(s)) => out.push_str(&json(&Value::String(s.clone()))),
        YamlOwned::Sequence(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                render_tree(item, out);
            }
            out.push(']');
        }
        YamlOwned::Mapping(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                match k {
                    YamlOwned::Value(ScalarOwned::String(s)) => {
                        out.push_str(&json(&Value::String(s.clone())))
                    }
                    other => {
                        out.push_str("<NONSTRING-KEY ");
                        render_tree(other, out);
                        out.push('>');
                    }
                }
                out.push(':');
                render_tree(v, out);
            }
            out.push('}');
        }
        YamlOwned::Tagged(tag, inner) => {
            let _ = write!(out, "<TAG {tag:?} ");
            render_tree(inner, out);
            out.push('>');
        }
        other => {
            let _ = write!(out, "<{other:?}>");
        }
    }
}

// ------------------------------------------------------------- the corpus

struct Doc {
    origin: String,
    text: String,
}

fn read_docs(paths: impl IntoIterator<Item = PathBuf>, label: &str) -> Vec<Doc> {
    paths
        .into_iter()
        .map(|path| Doc {
            origin: format!("{label}:{}", path.display()),
            text: std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
        })
        .collect()
}

fn yaml_files(dir: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                found.extend(yaml_files(&path, true));
            }
        } else if path
            .extension()
            .is_some_and(|e| e == "yaml" || e == "yml")
        {
            found.push(path);
        }
    }
    found.sort();
    found
}

fn is_graph(text: &str) -> bool {
    text.lines().any(|l| l.starts_with("members:"))
}

/// The fenced blocks of one language out of a markdown file.
fn fenced(markdown: &str, lang: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in markdown.lines() {
        match &mut current {
            None if line.trim_start() == format!("```{lang}") => current = Some(String::new()),
            Some(block) if line.trim_start() == "```" => {
                blocks.push(std::mem::take(block));
                current = None;
            }
            Some(block) => {
                block.push_str(line);
                block.push('\n');
            }
            None => {}
        }
    }
    blocks
}

struct Corpus {
    repo: PathBuf,
    orchestrator: PathBuf,
    graphs: Vec<Doc>,
    goldens: Vec<Doc>,
    schemaless: Vec<Doc>,
    filters: Vec<Doc>,
    workflows: Vec<Doc>,
    e2e_graph_paths: Vec<PathBuf>,
    real_graph_paths: Vec<PathBuf>,
    fixture_graph_paths: Vec<PathBuf>,
    persona_paths: Vec<PathBuf>,
    counts: BTreeMap<String, usize>,
}

fn corpus(repo: &Path, orchestrator: &Path, capture: Option<&Path>) -> Corpus {
    let mut counts = BTreeMap::new();
    let mut count = |k: &str, n: usize| {
        counts.insert(k.to_string(), n);
    };

    let goldens = read_docs(
        (4..=10).map(|v| repo.join(format!("tests/golden/graph.v{v}.yaml"))),
        "golden",
    );
    count("tests/golden/graph.v4-v10.yaml", goldens.len());

    let fixture_all = yaml_files(&repo.join("screenshots/fixture"), false);
    let fixture_graph_paths: Vec<PathBuf> = fixture_all
        .iter()
        .filter(|p| is_graph(&std::fs::read_to_string(p).unwrap_or_default()))
        .cloned()
        .collect();
    let fixture_graphs = read_docs(fixture_graph_paths.clone(), "fixture");
    let fixture_other = read_docs(
        fixture_all.iter().filter(|p| !fixture_graph_paths.contains(p)).cloned(),
        "fixture",
    );
    count("screenshots/fixture graphs", fixture_graphs.len());
    count("screenshots/fixture personas/bases", fixture_other.len());

    let mut contract_docs = Vec::new();
    for (file, label) in [("docs/contract.md", "contract.md"), ("README.md", "README.md")] {
        let md = std::fs::read_to_string(repo.join(file)).unwrap_or_default();
        for (i, block) in fenced(&md, "yaml").into_iter().enumerate() {
            if is_graph(&block) {
                contract_docs.push(Doc { origin: format!("{label} yaml block {}", i + 1), text: block });
            }
        }
    }
    count("docs/contract.md + README.md fenced graph blocks", contract_docs.len());

    let real_graph_paths = yaml_files(&orchestrator.join("graphs"), false);
    let real_graphs = read_docs(real_graph_paths.clone(), "ai-orchestrator");
    count("ai-orchestrator graphs/*.yaml", real_graphs.len());

    let mut e2e_graph_paths = Vec::new();
    let mut e2e_other = Vec::new();
    let mut e2e_filters = Vec::new();
    if let Some(capture) = capture {
        for path in yaml_files(capture, true) {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if is_graph(&text) {
                e2e_graph_paths.push(path);
            } else if name.contains("filter") {
                e2e_filters.push(Doc { origin: format!("e2e:{}", path.display()), text });
            } else if name != "llmlint.yml" {
                e2e_other.push(Doc { origin: format!("e2e:{}", path.display()), text });
            }
        }
    }
    let e2e_graphs = read_docs(e2e_graph_paths.clone(), "e2e");
    count("e2e-written graphs (captured)", e2e_graphs.len());
    count("e2e-written bases/personas (captured)", e2e_other.len());
    count("e2e-written --event-filter files (captured)", e2e_filters.len());

    let persona_paths: Vec<PathBuf> = yaml_files(&orchestrator.join("personas"), true);
    let real_personas = read_docs(persona_paths.clone(), "ai-orchestrator");
    count("ai-orchestrator personas/**/*.yaml", real_personas.len());
    // The onejudge base configs the real graphs name, and nothing else in
    // `config/`: those are the bases this crate reads.
    let mut bases: Vec<PathBuf> = real_graph_paths
        .iter()
        .filter_map(|g| {
            let v: Value = serde_norway::from_str(&std::fs::read_to_string(g).ok()?).ok()?;
            let dir = g.parent()?.to_path_buf();
            Some(
                v.get("members")?
                    .as_object()?
                    .values()
                    .filter_map(|m| m.get("base_config")?.as_str().map(|b| dir.join(b)))
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    bases.sort();
    bases.dedup();
    let real_config = read_docs(bases, "ai-orchestrator");
    count("ai-orchestrator onejudge base configs the real graphs name", real_config.len());
    let shipped = read_docs(yaml_files(&repo.join("personas"), true), "repo");
    count("repo personas/**/*.yaml (shipped)", shipped.len());

    let workflows = read_docs(yaml_files(&repo.join(".github/workflows"), false), "repo");
    count(".github/workflows/*.yml (workflow_contract walk)", workflows.len());

    let mut graphs = Vec::new();
    graphs.extend(goldens.iter().map(|d| Doc { origin: d.origin.clone(), text: d.text.clone() }));
    graphs.extend(fixture_graphs);
    graphs.extend(contract_docs);
    graphs.extend(real_graphs);
    graphs.extend(e2e_graphs);

    let mut schemaless = Vec::new();
    schemaless.extend(shipped);
    schemaless.extend(fixture_other);
    schemaless.extend(real_personas);
    schemaless.extend(real_config);
    schemaless.extend(e2e_other);

    // `--event-filter` files: the e2e suite's own, plus every graph's
    // `events.filter` block written out as the standalone file it would be.
    let mut filters = e2e_filters;
    for doc in &graphs {
        if let Ok(value) = norway::<Value>(&doc.text) {
            if let Some(filter) = value.pointer("/events/filter") {
                filters.push(Doc {
                    origin: format!("{} events.filter", doc.origin),
                    text: serde_norway::to_string(filter).unwrap(),
                });
            }
        }
    }
    count("--event-filter documents", filters.len());

    Corpus {
        repo: repo.to_path_buf(),
        orchestrator: orchestrator.to_path_buf(),
        graphs,
        goldens,
        schemaless,
        filters,
        workflows,
        e2e_graph_paths,
        real_graph_paths,
        fixture_graph_paths,
        persona_paths,
        counts,
    }
}

// ----------------------------------------------------------------- checks

fn which_profiles<T, F>(text: &str, expected: &Result<T, String>, matches: F) -> Vec<&'static str>
where
    T: serde::de::DeserializeOwned,
    F: Fn(&Result<T, String>, &Result<T, String>) -> bool,
{
    de_profiles()
        .into_iter()
        .filter(|(_, opts)| matches(expected, &saphyr_with::<T>(text, opts.clone())))
        .map(|(name, _)| name)
        .collect()
}

fn ok_eq_json(a: &Result<Value, String>, b: &Result<Value, String>) -> bool {
    match (a, b) {
        (Ok(a), Ok(b)) => json(a) == json(b),
        (Err(_), Err(_)) => true,
        _ => false,
    }
}

/// Both refused, with different text: a difference in what errors say.
fn err_text_differs<T>(a: &Result<T, String>, b: &Result<T, String>) -> bool {
    matches!((a, b), (Err(x), Err(y)) if x != y)
}

fn outcome<T: std::fmt::Debug>(r: &Result<T, String>) -> String {
    match r {
        Ok(v) => format!("Ok({v:?})"),
        Err(e) => format!("Err: {e}"),
    }
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}… [{} chars]", s.chars().take(n).collect::<String>(), s.chars().count())
    }
}

/// Graph read, typed and direct (`from_str::<GraphConfig>`, as the tests do).
fn graph_typed(c: &Corpus) -> Section {
    let mut s = Section::new("READ graph — typed, direct `from_str::<GraphConfig>`");
    for doc in &c.graphs {
        let a = norway::<GraphConfig>(&doc.text);
        let b = saphyr::<GraphConfig>(&doc.text);
        match (&a, &b) {
            (Ok(x), Ok(y)) if x == y => s.same(),
            (Err(x), Err(y)) if x == y => s.same(),
            (Err(x), Err(y)) => {
                *s.tally.entry("both refuse; error text differs".into()).or_default() += 1;
                s.diff(format!(
                    "{}\n  both refuse; error text differs:\n  serde_norway: {}\n  serde-saphyr: {}",
                    doc.origin,
                    short(x, 400),
                    short(&y.replace('\n', "⏎"), 400)
                ))
            }
            _ => s.diff(format!(
                "{}\n  serde_norway: {}\n  serde-saphyr: {}\n  profiles that agree: {:?}",
                doc.origin,
                short(&outcome(&a), 400),
                short(&outcome(&b), 400),
                which_profiles::<GraphConfig, _>(&doc.text, &a, |x, y| match (x, y) {
                    (Ok(x), Ok(y)) => x == y,
                    (Err(_), Err(_)) => true,
                    _ => false,
                })
            )),
        }
    }
    s
}

/// Graph read, production path: loose `Value` → (overrides) → typed.
fn graph_production(c: &Corpus) -> Section {
    let mut s = Section::new(
        "READ graph — production path (run.rs/main.rs): str → serde_json::Value → to_value/from_value",
    );
    for doc in &c.graphs {
        let loose_a = norway::<Value>(&doc.text);
        let loose_b = saphyr::<Value>(&doc.text);
        let mut problems = Vec::new();
        if !ok_eq_json(&loose_a, &loose_b) {
            problems.push(format!(
                "loose Value differs:\n    serde_norway: {}\n    serde-saphyr: {}",
                short(&loose_a.as_ref().map(json).unwrap_or_else(|e| e.clone()), 300),
                short(&loose_b.as_ref().map(json).unwrap_or_else(|e| e.clone()), 300)
            ));
        }
        let today = graph_norway_production(&doc.text);
        for (name, other) in [
            ("serde_json::from_value", graph_saphyr_json(&doc.text)),
            ("saphyr re-render + typed re-read", graph_saphyr_rerender(&doc.text)),
        ] {
            let agree = match (&today, &other) {
                (Ok(x), Ok(y)) => x == y,
                (Err(x), Err(y)) => x == y,
                _ => false,
            };
            if !agree {
                problems.push(format!(
                    "typed via {name}:\n    serde_norway: {}\n    serde-saphyr: {}",
                    short(&outcome(&today), 300),
                    short(&outcome(&other), 300)
                ));
            }
        }
        if problems.is_empty() {
            s.same();
        } else {
            s.diff(format!(
                "{}\n  {}\n  production-path profiles that agree: {:?}",
                doc.origin,
                problems.join("\n  "),
                production_profiles(&doc.text)
            ));
        }
    }
    s
}

/// `--set path=value`'s leaf, parsed loosely (`run.rs::apply_overrides`).
fn set_leaves() -> Section {
    let mut s = Section::new("READ --set leaf values — `from_str::<serde_json::Value>(value)` (run.rs)");
    let leaves = [
        "true", "false", "True", "TRUE", "yes", "no", "Yes", "on", "off", "On", "y", "n", "Y", "N",
        "null", "~", "Null", "NULL", "", " ", "0", "1", "-1", "+1", "010", "0o10", "0x1F", "0b101",
        "1_000", "1.5", "1e3", "1.0e+3", ".5", ".inf", "-.inf", ".nan", ".NaN", "1e999",
        "12:30", "12:30:45", "2026-10-12", "2026-10-12T10:00:00Z", "opus", "claude-opus-5-5",
        "./a.toml", "'quoted'", "\"double\"", "[a, b]", "[]", "{a: 1}", "{}", "a: b", "- a",
        "foo #comment", "#only", "a\nb", "|\n  block\n", "!!str 1", "!custom x", "&a 1", "*a",
        "<<: {a: 1}", "18446744073709551616", "-9223372036854775809", "1800", "30s", "@at",
        "`tick`", "%percent", "a,b", "key: [unterminated",
    ];
    for leaf in leaves {
        let a = norway::<Value>(leaf);
        let b = saphyr::<Value>(leaf);
        if ok_eq_json(&a, &b) {
            s.same();
            continue;
        }
        s.diff(format!(
            "--set x={leaf:?}\n  serde_norway: {}\n  serde-saphyr: {}\n  profiles that agree: {:?}",
            a.as_ref().map(json).map(|j| format!("Ok({j})")).unwrap_or_else(|e| format!("Err: {}", short(e, 200))),
            b.as_ref().map(json).map(|j| format!("Ok({j})")).unwrap_or_else(|e| format!("Err: {}", short(e, 200))),
            which_profiles::<Value, _>(leaf, &a, ok_eq_json),
        ));
    }
    s.note(format!(
        "{} leaf spellings probed. A leaf's parse error never reaches a user (run.rs takes `.ok()` and falls back to the text), so Err/Err pairs are counted identical whatever they say",
        leaves.len()
    ));
    s
}

/// `--set` on an absent leaf, replayed as `run.rs::apply_overrides` does it:
/// the parsed reading first (unless null or equal to the text), the text
/// second, and the first one the typed graph accepts wins. Today's typed check
/// is `to_value`/`from_value`; serde-saphyr's is `serde_json::from_value`.
fn set_candidates() -> Section {
    let mut s = Section::new("READ --set on an absent leaf — the candidate the schema accepts (run.rs replay)");
    let base = concat!(
        "version: 10\nname: g\nmembers:\n  w:\n    kind: onejudge\n    base_config: ./b.yaml\n",
        "    mode: bypass\n    agent: {oneharness_config: ./a.toml}\n",
        "    judge: {oneharness_config: ./j.toml}\n    judge_settings: {}\n",
        "  h:\n    kind: oneharness\n    oneharness_config: ./a.toml\n",
    );
    let paths: [(&[&str], &str); 6] = [
        (&["members", "w", "agent"], "model"),
        (&["members", "w"], "max_turns"),
        (&["members", "w"], "background"),
        (&["members", "w", "judge_settings"], "x"),
        (&["members", "h"], "task"),
        (&["members", "h"], "dir"),
    ];
    let leaves = [
        "yes", "on", "true", "5", "010", "1_000", "1.5", ".inf", "1e999", "[a, b]", "{a: 1}",
        "<<: {a: 1}", "18446744073709551616", "!custom x", "2026-10-12",
    ];
    type Parse = fn(&str) -> Result<Value, String>;
    type Typed = fn(&Value) -> bool;
    fn chosen(base: &Value, parents: &[&str], last: &str, leaf: &str, parse: Parse, typed: Typed) -> String {
        let written = Value::String(leaf.to_string());
        let mut candidates = Vec::new();
        if let Some(parsed) = parse(leaf).ok().filter(|p| !p.is_null() && p != &written) {
            candidates.push(parsed);
        }
        candidates.push(written);
        for candidate in candidates {
            let mut doc = base.clone();
            let mut cursor = &mut doc;
            for p in parents {
                cursor = cursor.get_mut(*p).unwrap();
            }
            cursor.as_object_mut().unwrap().insert(last.to_string(), candidate.clone());
            if typed(&doc) {
                return format!("accepted as {}", json(&candidate));
            }
        }
        "REFUSED: the schema has no field at this path, or the value does not parse as that field's type".into()
    }
    let doc: Value = norway(base).unwrap();
    for (parents, last) in paths {
        for leaf in leaves {
            let today = chosen(&doc, parents, last, leaf, |l| norway::<Value>(l), |d| {
                serde_norway::to_value(d)
                    .ok()
                    .and_then(|v| serde_norway::from_value::<GraphConfig>(v).ok())
                    .is_some()
            });
            let next = chosen(&doc, parents, last, leaf, |l| saphyr::<Value>(l), |d| {
                serde_json::from_value::<GraphConfig>(d.clone()).is_ok()
            });
            let closest = chosen(&doc, parents, last, leaf, |l| saphyr_with::<Value>(l, closest_options()), |d| {
                serde_json::from_value::<GraphConfig>(d.clone()).is_ok()
            });
            if today == next {
                s.same();
            } else {
                s.diff(format!(
                    "--set {}.{last}={leaf:?}\n  serde_norway:          {today}\n  serde-saphyr default:  {next}\n  serde-saphyr CLOSEST:  {closest}",
                    parents.join(".")
                ));
            }
        }
    }
    s
}

/// Tricky scalars inside a graph — through the internally tagged `Member`
/// (serde's buffered `Content`, i.e. `deserialize_any`) and the plain structs.
fn graph_scalar_probes() -> Section {
    let mut s = Section::new(
        "READ graph — scalar probes in string/number/bool fields (internally-tagged Member)",
    );
    let shapes: Vec<(&str, String)> = [
        "yes", "no", "on", "off", "y", "n", "true", "null", "~", "1", "1.0", "010", "0o17", "0x1F",
        "1e3", ".inf", "2026-10-12", "12:30", "'yes'", "\"1\"",
    ]
    .iter()
    .flat_map(|v| {
        vec![
            (
                "member `model:` (string)",
                format!(
                    "version: 10\nname: g\nmembers:\n  w:\n    kind: oneharness\n    oneharness_config: ./a.toml\n    model: {v}\n"
                ),
            ),
            (
                "graph `name:` (string)",
                format!(
                    "version: 10\nname: {v}\nmembers:\n  w:\n    kind: oneharness\n    oneharness_config: ./a.toml\n"
                ),
            ),
            (
                "graph `env:` value (string map)",
                format!(
                    "version: 10\nname: g\nenv:\n  K: {v}\nmembers:\n  w:\n    kind: oneharness\n    oneharness_config: ./a.toml\n"
                ),
            ),
            (
                "member `background:` (bool)",
                format!(
                    "version: 10\nname: g\nmembers:\n  w:\n    kind: oneharness\n    oneharness_config: ./a.toml\n    background: {v}\n"
                ),
            ),
            (
                "`version:` (u32)",
                format!(
                    "version: {v}\nname: g\nmembers:\n  w:\n    kind: oneharness\n    oneharness_config: ./a.toml\n"
                ),
            ),
        ]
    })
    .map(|(a, b)| (a, b))
    .collect();
    for (what, doc) in &shapes {
        let a = norway::<GraphConfig>(doc);
        let b = saphyr::<GraphConfig>(doc);
        let c_ = graph_norway_production(doc);
        let d = graph_saphyr_json(doc);
        let closest_prod = graph_saphyr_json_with(doc, closest_options());
        let typed_agree = match (&a, &b) {
            (Ok(x), Ok(y)) => x == y,
            (Err(_), Err(_)) => true,
            _ => false,
        };
        let prod_agree = match (&c_, &d) {
            (Ok(x), Ok(y)) => x == y,
            // The production path's refusal is what a user reads.
            (Err(x), Err(y)) => x.trim_start_matches("from_value: ") == y.trim_start_matches("from_value: "),
            _ => false,
        };
        if typed_agree && prod_agree {
            s.same();
            continue;
        }
        let closest_class = match (&c_, &closest_prod) {
            (Ok(x), Ok(y)) if x == y => "production path: CLOSEST reproduces today",
            (Err(x), Err(y)) if x.trim_start_matches("from_value: ") == y.trim_start_matches("from_value: ") => "production path: CLOSEST reproduces today",
            (Err(_), Err(_)) => "production path: both refuse under CLOSEST, error text differs",
            (Ok(_), Err(_)) => "production path: today accepts, CLOSEST refuses",
            (Err(_), Ok(_)) => "production path: today refuses, CLOSEST accepts",
            (Ok(_), Ok(_)) => "production path: both accept, CLOSEST reads a different value",
        };
        *s.tally.entry(format!("{closest_class}{}", if typed_agree { "" } else { "; typed-direct differs" })).or_default() += 1;
        let value_line = doc.lines().find(|l| l.contains(": ") && !l.contains("kind") && !l.contains("oneharness_config") && !l.starts_with("name: g") && !l.starts_with("version: 10")).unwrap_or("");
        s.diff(format!(
            "{what}, written `{}`\n  typed direct  — serde_norway: {}\n                  serde-saphyr: {}\n  production    — serde_norway: {}\n  (json path)     serde-saphyr: {}\n                  saphyr CLOSEST: {}\n  typed-direct profiles that agree: {:?}\n  production-path profiles that agree: {:?}",
            value_line.trim(),
            short(&outcome(&a.map(|g| field_probe(&g))), 160),
            short(&outcome(&b.map(|g| field_probe(&g))), 160),
            short(&outcome(&c_.map(|g| field_probe(&g))), 160),
            short(&outcome(&d.map(|g| field_probe(&g))), 160),
            short(&outcome(&closest_prod.clone().map(|g| field_probe(&g))), 160),
            which_profiles::<GraphConfig, _>(doc, &norway::<GraphConfig>(doc), |x, y| match (x, y) {
                (Ok(x), Ok(y)) => x == y,
                (Err(_), Err(_)) => true,
                _ => false,
            }),
            production_profiles(doc),
        ));
    }
    s
}

fn field_probe(g: &GraphConfig) -> String {
    let member = serde_json::to_value(&g.members["w"]).unwrap();
    format!(
        "version={} name={:?} env={:?} model={} background={}",
        g.version,
        g.name,
        g.env,
        member.get("model").map(json).unwrap_or_default(),
        member.get("background").map(json).unwrap_or_default()
    )
}

/// `--event-filter <file>` (`main.rs::requested_filter`).
fn event_filters(c: &Corpus) -> Section {
    let mut s = Section::new("READ --event-filter files — `from_str::<EventFilter>`");
    for doc in &c.filters {
        let a = norway::<EventFilter>(&doc.text);
        let b = saphyr::<EventFilter>(&doc.text);
        let agree = match (&a, &b) {
            (Ok(x), Ok(y)) => format!("{x:?}") == format!("{y:?}"),
            (Err(x), Err(y)) => x == y,
            _ => false,
        };
        if agree {
            s.same();
        } else if err_text_differs(&a, &b) {
            let x = a.as_ref().err().unwrap();
            let y = b.as_ref().err().unwrap();
            let plain = saphyr_with::<EventFilter>(&doc.text, no_snippet()).err().unwrap_or_default();
            s.diff(format!(
                "{}\n  both refuse; error text differs:\n  serde_norway:                       {}\n  serde-saphyr (default):             {}\n  serde-saphyr (with_snippet=false):  {}",
                doc.origin,
                short(x, 300),
                short(&y.replace('\n', "⏎"), 300),
                short(&plain, 300)
            ));
        } else {
            s.diff(format!(
                "{}\n  serde_norway: {}\n  serde-saphyr: {}",
                doc.origin,
                short(&outcome(&a), 300),
                short(&outcome(&b), 300)
            ));
        }
    }
    s
}

/// Persona files, onejudge base configs, and every other schema-less read
/// (`persona.rs` parse + merge): what lands in `serde_json::Value`, beside
/// saphyr's node tree.
fn schemaless(c: &Corpus, docs: &[Doc], title: &str) -> Section {
    let mut s = Section::new(title);
    let mut tree_mismatch = 0;
    for doc in docs {
        let a = norway::<Value>(&doc.text);
        let b = saphyr::<Value>(&doc.text);
        let tree = saphyr_tree(&doc.text);
        // The saphyr tree beside serde_norway's Value: a tree carrying something
        // the Value has no slot for is what would make the tree necessary.
        if let (Ok(a), Ok(t)) = (&a, &tree) {
            if json(a) != *t {
                tree_mismatch += 1;
                s.note(format!(
                    "{}: saphyr tree differs from serde_norway's Value: {}",
                    doc.origin,
                    first_divergence(&json(a), t)
                ));
            }
        } else if a.is_ok() != tree.is_ok() {
            s.note(format!(
                "{}: saphyr tree {} where serde_norway {}",
                doc.origin,
                if tree.is_ok() { "loads" } else { "fails" },
                if a.is_ok() { "loads" } else { "fails" }
            ));
        }
        if err_text_differs(&a, &b) {
            let plain = saphyr_with::<Value>(&doc.text, no_snippet()).err().unwrap_or_default();
            s.diff(format!(
                "{}\n  both refuse; error text differs:\n  serde_norway:                       {}\n  serde-saphyr (default):             {}\n  serde-saphyr (with_snippet=false):  {}",
                doc.origin,
                short(a.as_ref().err().unwrap(), 300),
                short(&b.as_ref().err().unwrap().replace('\n', "⏎"), 400),
                short(&plain, 300)
            ));
            continue;
        }
        if ok_eq_json(&a, &b) {
            s.same();
            continue;
        }
        s.diff(format!(
            "{}\n  serde_norway: {}\n  serde-saphyr: {}\n  first divergence: {}\n  profiles that agree: {:?}",
            doc.origin,
            short(&a.as_ref().map(json).unwrap_or_else(|e| format!("Err: {e}")), 200),
            short(&b.as_ref().map(json).unwrap_or_else(|e| format!("Err: {e}")), 200),
            match (&a, &b) {
                (Ok(x), Ok(y)) => first_divergence(&json(x), &json(y)),
                _ => String::new(),
            },
            which_profiles::<Value, _>(&doc.text, &a, ok_eq_json),
        ));
    }
    s.note(format!(
        "saphyr 0.2 node tree vs serde_norway Value: {} of {} documents differ",
        tree_mismatch,
        docs.len()
    ));
    let _ = c;
    s
}

fn first_divergence(a: &str, b: &str) -> String {
    let at = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    let from = at.saturating_sub(60);
    let pick = |s: &str| s.chars().skip(from).take(160).collect::<String>();
    format!("at char {at}:\n      serde_norway: …{}\n      other:        …{}", pick(a), pick(b))
}

/// Schema-less walk probes: what each reader puts in `serde_json::Value` for
/// the YAML features a hand-written persona or workflow can use.
fn schemaless_probes() -> Section {
    let mut s = Section::new("READ schema-less — feature probes into serde_json::Value and saphyr's tree");
    let probes = [
        ("anchor + alias", "a: &x {k: 1}\nb: *x\n"),
        ("merge key", "base: &b {k: 1, j: 2}\nchild:\n  <<: *b\n  j: 3\n"),
        ("duplicate key", "k: 1\nk: 2\n"),
        ("integer key", "1: a\n2: b\n"),
        ("bool/null keys", "true: a\nnull: b\non: c\n"),
        ("explicit tag", "k: !!str 123\nj: !custom v\n"),
        ("yes/no/on/off values", "a: yes\nb: no\nc: on\nd: off\n"),
        ("octal-looking", "a: 010\nb: 0o10\nc: 0x10\n"),
        ("timestamp-looking", "a: 2026-10-12\nb: 12:30:00\n"),
        ("float specials", "a: .inf\nb: .nan\nc: 1e999\n"),
        ("big int", "a: 18446744073709551616\nb: 9223372036854775808\n"),
        ("literal block, keep/strip", "a: |+\n  x\n\nb: |-\n  y\nc: >\n  folded\n  text\n"),
        ("empty document", ""),
        ("comment only", "# nothing\n"),
        ("multi document", "a: 1\n---\nb: 2\n"),
        ("explicit doc markers", "---\na: 1\n...\n"),
        ("tab indentation", "a:\n\tb: 1\n"),
        ("trailing spaces in plain", "a: x   \n"),
        ("unicode escapes", "a: \"\\u00e9\\x41\\t\"\n"),
        ("BOM", "\u{feff}a: 1\n"),
        ("GitHub `on:` key", "on:\n  pull_request:\n    branches: [main]\n"),
        ("quoted key", "\"a b\": 1\n'c': 2\n"),
        ("persona bool flag spelled yes", "user:\n  done_when: x\n  done_when_replaces_base: yes\n"),
        ("null forms", "a:\nb: ~\nc: null\nd: Null\n"),
    ];
    for (what, text) in probes {
        let a = norway::<Value>(text);
        let b = saphyr::<Value>(text);
        let tree = saphyr_tree(text);
        let show = |r: &Result<Value, String>| {
            r.as_ref()
                .map(|v| format!("Ok({})", json(v)))
                .unwrap_or_else(|e| format!("Err: {}", short(&e.replace('\n', "⏎"), 220)))
        };
        let tree_s = tree.clone().map(|t| format!("Ok({t})")).unwrap_or_else(|e| format!("Err: {e}"));
        if ok_eq_json(&a, &b) && a.as_ref().map(json).ok() == tree.as_ref().ok().cloned() {
            s.same();
            continue;
        }
        s.diff(format!(
            "{what}: {text:?}\n  serde_norway → Value: {}\n  serde-saphyr → Value: {}\n  saphyr tree:          {}\n  serde-saphyr profiles that agree with serde_norway: {:?}",
            show(&a),
            show(&b),
            tree_s,
            which_profiles::<Value, _>(text, &a, ok_eq_json),
        ));
    }
    s
}

// ----------------------------------------------------------------- writes

fn ser_compare<T: serde::Serialize>(
    s: &mut Section,
    origin: &str,
    value: &T,
    today: &str,
    reread: impl Fn(&str) -> Result<String, String>,
) {
    let mut exact = Vec::new();
    let mut best: Option<(&str, String, usize)> = None;
    let mut default_out = String::new();
    for (name, opts) in ser_profiles() {
        let out = match saphyr_out(value, opts) {
            Ok(o) => o,
            Err(e) => {
                s.note(format!("{origin}: profile {name} fails to serialize: {e}"));
                continue;
            }
        };
        if name == "default" {
            default_out = out.clone();
        }
        if out == today {
            exact.push(name);
        }
        let changed = line_diff(today, &out, 0)
            .lines()
            .last()
            .and_then(|l| l.trim().strip_prefix('(').and_then(|r| r.split(' ').next()).and_then(|n| n.parse().ok()))
            .unwrap_or(usize::MAX);
        if best.as_ref().is_none_or(|(_, _, c)| changed < *c) {
            best = Some((name, out, changed));
        }
    }
    // The one code change tried on emitted text: a whitespace-only line becomes
    // empty (serde-saphyr indents the blank lines of a block scalar; serde_norway
    // does not). Unsafe in general — a block scalar line of only spaces beyond
    // its indentation is content — so it is reported, not assumed.
    let mut post = Vec::new();
    let mut post_reads = true;
    for (name, opts) in ser_profiles() {
        if let Ok(out) = saphyr_out(value, opts) {
            let stripped = strip_blank_indent(&out);
            if stripped == today {
                post.push(name);
                post_reads &= reread(&stripped) == reread(today);
            }
        }
    }
    let read_today = reread(today);
    let read_saphyr = reread(&default_out);
    let semantic = match (&read_today, &read_saphyr) {
        (Ok(a), Ok(b)) if a == b => "reads back identically".to_string(),
        // A document both readers refuse: the same refusal, but the position
        // moved with the layout.
        (Err(a), Err(b)) if strip_position(a) == strip_position(b) => {
            "reads back identically (both refused with the same message apart from line/column)".to_string()
        }
        (a, b) => format!(
            "READS BACK DIFFERENTLY\n    today:   {}\n    saphyr:  {}",
            short(&outcome(a), 300),
            short(&outcome(b), 300)
        ),
    };
    if exact.contains(&"default") {
        s.same();
        return;
    }
    *s.tally
        .entry(format!(
            "byte-identical under {exact:?}; after blank-line post-process under {post:?}; residual after both: {}; default output {}",
            residual(today, value),
            if semantic.starts_with("reads back identically") { "reads back identically" } else { "READS BACK DIFFERENTLY" }
        ))
        .or_default() += 1;
    let (best_name, best_out, _) = best.expect("a profile");
    s.diff(format!(
        "{origin}\n  byte-identical under profiles: {exact:?}\n  byte-identical after blank-line post-process under profiles: {post:?} (post-processed text reads back identically: {post_reads})\n  default profile output {semantic}\n  default profile diff:\n{}\n  closest profile `{best_name}` diff:\n{}",
        indent(&line_diff(today, &default_out, 40)),
        indent(&line_diff(today, &best_out, 40)),
    ));
}

fn strip_position(e: &str) -> String {
    match e.find(" at line ") {
        Some(at) => e[..at].to_string(),
        None => e.to_string(),
    }
}

/// What is left between today's bytes and serde-saphyr's best configuration
/// (`folded_wrap_chars=MAX+min_fold_chars=MAX`, then the blank-line
/// post-process): `quote style only` when every remaining line pair differs
/// only in which quote character wraps a scalar.
fn residual(today: &str, value: &impl serde::Serialize) -> String {
    let mut o = SerializerOptions::default();
    o.folded_wrap_chars = usize::MAX;
    o.min_fold_chars = usize::MAX;
    let Ok(out) = saphyr_out(value, o) else { return "unserializable".into() };
    let out = strip_blank_indent(&out);
    if out == today {
        return "none".into();
    }
    let a: Vec<&str> = today.lines().collect();
    let b: Vec<&str> = out.lines().collect();
    if a.len() != b.len() {
        return "line structure".into();
    }
    let quoteless = |l: &str| l.replace(['\'', '"'], "Q").replace("QQ", "Q");
    if a.iter().zip(&b).all(|(x, y)| x == y || quoteless(x) == quoteless(y)) {
        "quote style only ('…' today, \"…\" serde-saphyr)".into()
    } else {
        "other".into()
    }
}

fn strip_blank_indent(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|l| {
            let body = l.strip_suffix('\n').unwrap_or(l);
            if !body.is_empty() && body.chars().all(|c| c == ' ') {
                if l.ends_with('\n') { "\n" } else { "" }
            } else {
                l
            }
        })
        .collect()
}

/// One small value per class of emitted difference, written by both crates,
/// so each class has its before and after bytes on record.
fn emitted_examples() -> Section {
    let mut s = Section::new("WRITE emitted-byte examples — one per class seen in the real writes");
    let long = "every acceptance criterion stated in the task is met, with every change this dispatch made committed and nothing half-applied left behind";
    let path = "/home/nick/.onevcs/workspaces/github.com-nickderobertis-oneagentgraph-f0db8e1bb310/pool/1/worktree/target/debug/oneagentgraph-fake-harness";
    let cases: Vec<(&str, Value)> = vec![
        ("blank line inside a literal block scalar", serde_json::json!({"system_prompt": "first paragraph\n\nsecond paragraph\n"})),
        ("nested: blank line inside a literal block under a mapping", serde_json::json!({"user": {"persona": "one\n\ntwo\n"}})),
        ("single-line string longer than 80 columns, with spaces", serde_json::json!({"done_when": long})),
        ("single-line string longer than 80 columns, no spaces", serde_json::json!({"ONEHARNESS_BIN_CLAUDE_CODE": path})),
        ("string that looks like an integer", serde_json::json!({"ONEHARNESS_HISTORY": "1"})),
        ("string containing `: `", serde_json::json!({"system_prompt": "Standing bar: verify before you claim done."})),
        ("string starting with `{`", serde_json::json!({"task": "{task}"})),
        ("empty string", serde_json::json!({"dir": ""})),
        ("string that looks like a YAML 1.1 boolean", serde_json::json!({"a": "yes", "b": "on"})),
        ("string that looks like null", serde_json::json!({"a": "null", "b": "~"})),
        ("list under a mapping key", serde_json::json!({"judge": [{"oneharness_config": "./j.toml"}, {"command": ["x"]}]})),
        ("empty mapping and empty list", serde_json::json!({"settings": {}, "deps": []})),
        ("null value", serde_json::json!({"task": null})),
        ("literal block with trailing blank lines", serde_json::json!({"a": "x\n\n\n"})),
        ("multi-line string without a trailing newline", serde_json::json!({"a": "x\ny"})),
    ];
    for (what, value) in cases {
        let today = serde_norway::to_string(&value).unwrap();
        let next = saphyr_out(&value, SerializerOptions::default()).unwrap();
        let mut o = SerializerOptions::default();
        o.folded_wrap_chars = usize::MAX;
        o.min_fold_chars = usize::MAX;
        let best = strip_blank_indent(&saphyr_out(&value, o).unwrap());
        if today == next {
            s.same();
            continue;
        }
        let reproducing: Vec<String> = ser_profiles()
            .into_iter()
            .flat_map(|(name, o)| {
                let out = saphyr_out(&value, o).unwrap();
                let mut hits = Vec::new();
                if out == today {
                    hits.push(name.to_string());
                }
                if strip_blank_indent(&out) == today {
                    hits.push(format!("{name} + blank-line post-process"));
                }
                hits
            })
            .collect();
        s.diff(format!(
            "{what}\n  serde_norway bytes:  {today:?}\n  serde-saphyr bytes:  {next:?}\n  serde-saphyr with folded_wrap_chars=MAX+min_fold_chars=MAX, then the blank-line post-process: {best:?}\n  that reproduces today: {}\n  every profile (± post-process) that reproduces today: {reproducing:?}\n  serde-saphyr bytes read back by serde_norway to the same value: {}",
            best == today,
            serde_norway::from_str::<Value>(&next).map(|v| json(&v) == json(&value)).unwrap_or(false),
        ));
    }
    s
}

/// The graph goldens `tests/record.rs` compares byte for byte.
fn graph_writes(c: &Corpus) -> Section {
    let mut s = Section::new("WRITE graph records — goldens tests/record.rs compares byte for byte");
    for doc in &c.goldens {
        let graph: GraphConfig = norway(&doc.text).expect("a golden reads");
        let today = serde_norway::to_string(&graph).unwrap();
        if today != doc.text {
            s.note(format!("{}: serde_norway itself does not reproduce the golden", doc.origin));
        }
        ser_compare(&mut s, &doc.origin, &graph, &doc.text, |t| {
            saphyr::<GraphConfig>(t)
                .or_else(|_| norway::<GraphConfig>(t))
                .map(|g| format!("{g:?}"))
        });
    }
    // The other documents a test serializes: every graph in the corpus.
    for doc in c.graphs.iter().filter(|d| !d.origin.starts_with("golden")) {
        let Ok(graph) = norway::<GraphConfig>(&doc.text) else { continue };
        let today = serde_norway::to_string(&graph).unwrap();
        ser_compare(&mut s, &format!("{} (re-serialized)", doc.origin), &graph, &today, |t| {
            saphyr::<GraphConfig>(t).map(|g| format!("{g:?}"))
        });
    }
    s
}

/// The indentation-sensitive substrings `src/config.rs` and `tests/contract.rs` assert.
fn indentation_substrings() -> Section {
    let mut s = Section::new(
        "WRITE indentation-sensitive substrings — src/config.rs and tests/contract.rs assertions",
    );
    let single = concat!(
        "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
        "    base_config: ./b.yaml\n    mode: bypass\n",
        "    agent: {oneharness_config: ./a.toml}\n",
        "    judge: {oneharness_config: ./j.toml}\n",
    );
    let stacked = concat!(
        "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n",
        "    base_config: ./b.yaml\n    mode: bypass\n",
        "    agent: {oneharness_config: ./a.toml}\n",
        "    judge:\n",
        "      - oneharness_config: ./j.toml\n        model: opus\n        label: reviewer\n",
        "      - kind: llmlint\n        config: ./llmlint.yml\n        bin: /opt/llmlint\n",
        "        diff_base: origin/main\n        args: [--rule, no_todo]\n        label: lint\n",
        "      - command: [my-judge, --flag]\n",
    );
    type Check = Box<dyn Fn(&str) -> bool>;
    let cases: Vec<(&str, &str, bool, Check)> = vec![
        (
            "config.rs:1686 one judge writes back as a mapping",
            single,
            false,
            Box::new(|w: &str| w.contains("    judge:\n      oneharness_config: ./j.toml\n")),
        ),
        (
            "config.rs:1735 a judge list writes back as a compact list",
            stacked,
            false,
            Box::new(|w: &str| w.contains("    judge:\n    - oneharness_config: ./j.toml\n") && w.contains("kind: llmlint\n")),
        ),
        (
            "contract.rs:2876 one side writes back as the mapping (member alone)",
            single,
            true,
            Box::new(|w: &str| w.contains("judge:\n  ") && !w.contains("judge:\n  - ")),
        ),
    ];
    for (what, doc, member_alone, check) in cases {
        let graph: GraphConfig = norway(doc).expect("parses");
        let today = if member_alone {
            serde_norway::to_string(&graph.members["w"]).unwrap()
        } else {
            serde_norway::to_string(&graph).unwrap()
        };
        assert!(check(&today), "serde_norway no longer satisfies {what}");
        let mut passing = Vec::new();
        let mut default_out = String::new();
        for (name, opts) in ser_profiles() {
            let out = if member_alone {
                saphyr_out(&graph.members["w"], opts).unwrap()
            } else {
                saphyr_out(&graph, opts).unwrap()
            };
            if name == "default" {
                default_out = out.clone();
            }
            if check(&out) {
                passing.push(name);
            }
        }
        let reread_ok = if member_alone {
            true
        } else {
            saphyr::<GraphConfig>(&default_out).map(|g| g == graph).unwrap_or(false)
        };
        if passing.contains(&"default") && default_out == today {
            s.same();
        } else {
            s.diff(format!(
                "{what}\n  assertion holds under profiles: {passing:?}\n  default output re-reads to the same graph: {reread_ok}\n  serde_norway → serde-saphyr (default):\n{}",
                indent(&line_diff(&today, &default_out, 30))
            ));
        }
    }
    s
}

/// The effective `onejudge.yaml` `invoke.rs` writes, per real two-party member.
fn effective_configs(c: &Corpus) -> Section {
    let mut s = Section::new(
        "WRITE effective onejudge.yaml — invoke.rs, per member, read back by onejudge's Config",
    );
    let scratch_root = tempfile::tempdir().expect("scratch");
    let mut built = 0;
    let mut members_seen = 0;
    let mut skipped_invalid = 0;
    let mut graph_sources: Vec<(String, PathBuf, Option<PathBuf>)> = Vec::new();
    for p in &c.real_graph_paths {
        graph_sources.push((format!("ai-orchestrator:{}", p.display()), p.clone(), None));
        // onepipeline forwards a dispatched node's persona as the worker's
        // persona override: every real persona through the node-scope worker.
        if p.file_name().is_some_and(|n| n == "node-scope.yaml") {
            for persona in &c.persona_paths {
                graph_sources.push((
                    format!(
                        "ai-orchestrator:{} + persona {}",
                        p.display(),
                        persona.strip_prefix(&c.orchestrator).unwrap_or(persona).display()
                    ),
                    p.clone(),
                    Some(persona.clone()),
                ));
            }
        }
    }
    for p in c.fixture_graph_paths.iter().chain(&c.e2e_graph_paths) {
        graph_sources.push((p.display().to_string(), p.clone(), None));
    }
    for (i, (origin, path, persona)) in graph_sources.iter().enumerate() {
        let text = std::fs::read_to_string(path).unwrap();
        let Ok(mut loose) = norway::<Value>(&text) else { continue };
        if let Some(persona) = persona {
            for (_, member) in loose["members"].as_object_mut().unwrap().iter_mut() {
                if member["kind"] == "onejudge" {
                    member["persona"] = Value::String(persona.display().to_string());
                }
            }
        }
        let Ok(graph) = serde_json::from_value::<GraphConfig>(loose) else { continue };
        // Built only where `run` would build: after `validate` and `ready_order`
        // accept the graph. (The e2e suite writes graphs meant to be refused
        // there — a member depending on itself overflows `invoke::build`.)
        if oneagentgraph::config::validate(&graph).is_err() || oneagentgraph::run::ready_order(&graph).is_err() {
            skipped_invalid += 1;
            continue;
        }
        for (name, member) in &graph.members {
            if !matches!(member, oneagentgraph::config::Member::Onejudge(_)) {
                continue;
            }
            members_seen += 1;
            let scratch = scratch_root.path().join(format!("{i}-{name}"));
            std::fs::create_dir_all(&scratch).unwrap();
            let dir = path.parent().unwrap();
            let context = oneagentgraph::invoke::Context {
                dir,
                scratch: &scratch,
                graph_dir: Some(dir),
                personas: graph.personas.as_deref(),
                task: Some("the spike's task"),
                task_text: TaskText::under(graph.version),
                session: "spike-session",
                oneharness_bin: "oneharness",
                background: graph.is_background(name),
            };
            let mut resolver = oneagentgraph::resolve::Resolver::new();
            let invocation = match oneagentgraph::invoke::build(member, &context, &mut resolver) {
                Ok(inv) => inv,
                Err(e) => {
                    s.note(format!("{origin} member {name}: does not build today ({})", short(&e.to_string(), 160)));
                    continue;
                }
            };
            let oneagentgraph::invoke::Launch::Judge(launch) = invocation.launch else { continue };
            built += 1;
            let today = std::fs::read_to_string(&launch.config).unwrap();
            let value: Value = norway(&today).expect("the effective config re-reads");
            if serde_norway::to_string(&value).unwrap() != today {
                s.note(format!("{origin} member {name}: serde_norway Value round trip is lossy"));
            }
            ser_compare(&mut s, &format!("{origin} member {name}"), &value, &today, |t| {
                onejudge::cli::Config::from_yaml(t)
                    .map(|c| format!("{c:?}"))
                    .map_err(|e| e.to_string())
            });
        }
    }
    s.note(format!(
        "{} graphs considered; {skipped_invalid} skipped as `validate`/`ready_order` refuse them; {members_seen} two-party members seen, {built} built and compared",
        graph_sources.len()
    ));
    s
}

// ----------------------------------------------------------------- errors

fn error_messages(c: &Corpus) -> Section {
    let mut s = Section::new(
        "ERRORS — serde_fields (`expected one of`, ` at line `), unknown fields, malformed documents",
    );
    let contract = std::fs::read_to_string(c.repo.join("docs/contract.md")).unwrap();
    let graph = fenced(&contract, "yaml").into_iter().find(|b| is_graph(b)).expect("contract graph");
    let mut cases: Vec<(String, String, &'static str)> = vec![
        (
            "serde_fields::<OneharnessMember>".into(),
            "oneharness_config: ./a.toml\nbogus_field_for_the_gate: 1\n".into(),
            "OneharnessMember",
        ),
        (
            "serde_fields::<OnejudgeMember>".into(),
            concat!(
                "base_config: ./b.yaml\nagent: {oneharness_config: ./a.toml}\n",
                "judge: {oneharness_config: ./j.toml}\nmode: bypass\n",
                "bogus_field_for_the_gate: 1\n"
            )
            .into(),
            "OnejudgeMember",
        ),
        ("graph unknown top-level `nodes`".into(), format!("{}\nnodes: []\n", graph.trim_end()), "GraphConfig"),
        (
            "onejudge member typo `moed`".into(),
            graph.replace("    mode: bypass", "    moed: bypass"),
            "GraphConfig",
        ),
        ("oneharness member typo `dpes`".into(), graph.replace("    deps: []", "    dpes: []"), "GraphConfig"),
        ("missing field `name`".into(), "version: 1\nmembers: {}\n".into(), "GraphConfig"),
        ("wrong type `version: x`".into(), "version: x\nname: g\nmembers: {}\n".into(), "GraphConfig"),
        (
            "unknown member kind".into(),
            "version: 1\nname: g\nmembers:\n  w:\n    kind: nope\n".into(),
            "GraphConfig",
        ),
        ("not YAML (unterminated flow)".into(), "version: 1\nname: [g\n".into(), "GraphConfig"),
        ("tab indentation".into(), "version: 1\nname: g\nmembers:\n\tw: {}\n".into(), "GraphConfig"),
        (
            "judge none of the three shapes".into(),
            "version: 1\nname: g\nmembers:\n  w:\n    kind: onejudge\n    base_config: ./b.yaml\n    mode: bypass\n    agent: {oneharness_config: ./a.toml}\n    judge: {model: x}\n".into(),
            "GraphConfig",
        ),
        ("duplicate member".into(), "version: 1\nname: g\nmembers:\n  w: {kind: oneharness, oneharness_config: ./a.toml}\n  w: {kind: oneharness, oneharness_config: ./b.toml}\n".into(), "GraphConfig"),
        ("empty document".into(), String::new(), "GraphConfig"),
    ];
    for p in [
        c.repo.join("screenshots/fixture/graph.broken.yaml"),
        c.repo.join("screenshots/fixture/persona.broken.yaml"),
    ] {
        if let Ok(t) = std::fs::read_to_string(&p) {
            cases.push((format!("fixture {}", p.display()), t, if is_graph_path(&p) { "GraphConfig" } else { "Value" }));
        }
    }
    for (what, doc, ty) in cases {
        let (a, b, plain) = match ty {
            "OneharnessMember" => (
                norway::<OneharnessMember>(&doc).map(|_| ()),
                saphyr::<OneharnessMember>(&doc).map(|_| ()),
                saphyr_with::<OneharnessMember>(&doc, no_snippet()).map(|_| ()),
            ),
            "OnejudgeMember" => (
                norway::<OnejudgeMember>(&doc).map(|_| ()),
                saphyr::<OnejudgeMember>(&doc).map(|_| ()),
                saphyr_with::<OnejudgeMember>(&doc, no_snippet()).map(|_| ()),
            ),
            "Value" => (
                norway::<Value>(&doc).map(|_| ()),
                saphyr::<Value>(&doc).map(|_| ()),
                saphyr_with::<Value>(&doc, no_snippet()).map(|_| ()),
            ),
            _ => (
                norway::<GraphConfig>(&doc).map(|_| ()),
                saphyr::<GraphConfig>(&doc).map(|_| ()),
                saphyr_with::<GraphConfig>(&doc, no_snippet()).map(|_| ()),
            ),
        };
        let prod_a = if ty == "GraphConfig" { Some(graph_norway_production(&doc).map(|_| ())) } else { None };
        let prod_b = if ty == "GraphConfig" { Some(graph_saphyr_json(&doc).map(|_| ())) } else { None };
        let prod_c = if ty == "GraphConfig" { Some(graph_saphyr_json_with(&doc, closest_options()).map(|_| ())) } else { None };
        // The one code change tried on this path: an empty document read as an
        // empty mapping before `from_value`, as serde_norway's `from_value` does.
        let prod_d = if ty == "GraphConfig" {
            Some(
                saphyr_with::<Value>(&doc, closest_options())
                    .map_err(|e| format!("parse: {e}"))
                    .and_then(|v| {
                        let v = if v.is_null() { Value::Object(Default::default()) } else { v };
                        serde_json::from_value::<GraphConfig>(v).map(|_| ()).map_err(|e| format!("from_value: {e}"))
                    }),
            )
        } else {
            None
        };
        // `tests/contract.rs::serde_fields`, verbatim in what it does to the text.
        let fields = |e: &str| -> String {
            let Some((_, listed)) = e.split_once("expected one of ") else {
                return "serde_fields PANICS: no `expected one of`".into();
            };
            let listed = listed.split(" at line ").next().unwrap_or(listed);
            let fields: Vec<String> =
                listed.split(',').map(|f| f.trim().trim_matches('`').to_string()).collect();
            let clean = fields
                .iter()
                .all(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
            if clean && fields.len() > 3 {
                format!("serde_fields PASSES with {fields:?}")
            } else {
                format!("serde_fields FAILS its own assertion with {:?}", fields.iter().map(|f| short(f, 60)).collect::<Vec<_>>())
            }
        };
        let same = a == b && prod_a == prod_b;
        let text = format!(
            "{what}\n  serde_norway:                     {}\n  serde-saphyr (default, snippet):  {}\n  serde-saphyr (with_snippet=false): {}{}{}",
            show_err(&a),
            show_err(&b),
            show_err(&plain),
            if what.starts_with("serde_fields") {
                format!(
                    "\n  serde_fields over serde_norway:            {}\n  serde_fields over serde-saphyr (default):  {}\n  serde_fields over serde-saphyr (plain):    {}\n  ` at line ` present — serde_norway: {}, saphyr default: {}, saphyr plain: {}",
                    a.as_ref().err().map(|e| fields(e)).unwrap_or_default(),
                    b.as_ref().err().map(|e| fields(e)).unwrap_or_default(),
                    plain.as_ref().err().map(|e| fields(e)).unwrap_or_default(),
                    a.as_ref().err().is_some_and(|e| e.contains(" at line ")),
                    b.as_ref().err().is_some_and(|e| e.contains(" at line ")),
                    plain.as_ref().err().is_some_and(|e| e.contains(" at line ")),
                )
            } else {
                String::new()
            },
            match (&prod_a, &prod_b, &prod_c, &prod_d) {
                (Some(x), Some(y), Some(z), Some(w)) => format!(
                    "\n  production path — serde_norway (to_value/from_value):  {}\n                    serde-saphyr + serde_json::from_value:   {}\n                    serde-saphyr CLOSEST + from_value:        {}\n                    CLOSEST + null-as-{{}} + from_value:      {}",
                    show_err(x),
                    show_err(y),
                    show_err(z),
                    show_err(w)
                ),
                _ => String::new(),
            }
        );
        if same {
            s.same();
        } else {
            s.diff(text);
        }
    }
    s
}

fn is_graph_path(p: &Path) -> bool {
    is_graph(&std::fs::read_to_string(p).unwrap_or_default())
}

fn no_snippet() -> Options {
    let mut o = Options::default();
    o.with_snippet = false;
    o
}

fn show_err(r: &Result<(), String>) -> String {
    match r {
        Ok(()) => "Ok".into(),
        Err(e) => format!("Err: {}", short(&e.replace('\n', "⏎"), 500)),
    }
}

// ----------------------------------------------------------------- timing

fn timing(c: &Corpus) {
    println!("\n{}\n## PARSE TIME — largest real documents\n", "=".repeat(78));
    // The largest document oneagentgraph reads at run time (a graph, persona or
    // base), and the largest YAML file tracked in this repository.
    let mut runtime: Vec<&Doc> = c.graphs.iter().chain(&c.schemaless).collect();
    runtime.retain(|d| !d.origin.starts_with("e2e"));
    runtime.sort_by_key(|d| std::cmp::Reverse(d.text.len()));
    let mut tracked: Vec<&Doc> = c
        .graphs
        .iter()
        .chain(&c.schemaless)
        .chain(&c.workflows)
        .filter(|d| d.origin.starts_with("repo:") || d.origin.starts_with("golden:") || d.origin.starts_with("fixture:"))
        .collect();
    tracked.sort_by_key(|d| std::cmp::Reverse(d.text.len()));
    for (label, doc) in [("largest document read at run time", runtime[0]), ("largest YAML file tracked in this repository", tracked[0])] {
        println!("{label}: {} ({} bytes, {} lines)", doc.origin, doc.text.len(), doc.text.lines().count());
        time_one(&doc.text);
    }
}

fn time_one(text: &str) {
    let runs = 500;
    let time = |f: &dyn Fn()| {
        for _ in 0..20 {
            f();
        }
        let mut samples: Vec<f64> = (0..runs)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        samples.sort_by(f64::total_cmp);
        (samples[0], samples[runs / 2], samples[runs * 9 / 10])
    };
    let closest = closest_options();
    for (name, f) in [
        ("serde_norway 0.9 → serde_json::Value", Box::new(|| {
            std::hint::black_box(serde_norway::from_str::<Value>(text).unwrap());
        }) as Box<dyn Fn()>),
        ("serde-saphyr 1.3 (CLOSEST) → serde_json::Value", Box::new(move || {
            std::hint::black_box(serde_saphyr::from_str_with_options::<Value>(text, closest.clone()).unwrap());
        })),
        ("saphyr 0.2 → YamlOwned tree", Box::new(|| {
            std::hint::black_box(saphyr::YamlOwned::load_from_str(text).unwrap());
        })),
    ] {
        let (min, median, p90) = time(&*f);
        println!("  {name:<48} min {min:>8.1} µs   median {median:>8.1} µs   p90 {p90:>8.1} µs   ({runs} runs, release)");
    }
}

// ------------------------------------------------------------------- main

fn main() {
    let mut args = std::env::args().skip(1);
    let mut orchestrator = PathBuf::from("/home/nick/projects/ai-orchestrator");
    let mut capture = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--orchestrator" => orchestrator = PathBuf::from(args.next().expect("a dir")),
            "--e2e-capture" => capture = Some(PathBuf::from(args.next().expect("a dir"))),
            other => panic!("unknown argument {other}"),
        }
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let c = corpus(&repo, &orchestrator, capture.as_deref());

    println!("# serde_norway 0.9 vs serde-saphyr 1.3 (+ saphyr 0.2 tree) — differential report");
    println!("\ncrate versions: see `cargo tree` in run.sh output; corpus:");
    for (k, v) in &c.counts {
        println!("  {v:>4}  {k}");
    }
    println!("\nCLOSEST reader profile = {:?}", closest_options());

    graph_typed(&c).print();
    graph_production(&c).print();
    graph_scalar_probes().print();
    set_leaves().print();
    set_candidates().print();
    event_filters(&c).print();
    schemaless(&c, &c.schemaless, "READ persona files + onejudge base configs — `from_str::<serde_json::Value>` (persona.rs)").print();
    schemaless(&c, &c.workflows, "READ .github/workflows — schema-less walk (tests/repo-tooling/workflow_contract.rs)").print();
    schemaless_probes().print();
    emitted_examples().print();
    graph_writes(&c).print();
    indentation_substrings().print();
    effective_configs(&c).print();
    error_messages(&c).print();
    timing(&c);
}
