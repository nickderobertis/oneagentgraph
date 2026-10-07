//! What the committed workflows schedule, for the events that decide it.
//!
//! Main's branch protection requires a fixed set of status-check contexts by
//! name, and the staged gate decides which tier runs where: the affected tier on
//! every pull request and every push to main, the full sweep once, on the
//! release-plz release PR. Both are properties of the `on`, `if`, `needs`,
//! `strategy`, and `runs-on` fields GitHub reads, so this parses every workflow
//! and evaluates those fields the way GitHub does for synthetic events — an
//! ordinary pull request, a release-plz release PR, a fork's pull request, a push
//! to main — with the `changes` job answering either way.
//!
//! That is a structural check rather than an end-to-end one, on purpose: the
//! behaviour is GitHub's scheduling of hosted runners, whose only end-to-end
//! proof is such a pull request. What this repository authors is exactly the
//! fields read here, and an expression the evaluator cannot read fails the test
//! rather than reading as whichever outcome a case wanted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_norway::Value;

/// This checkout, whose committed files are what is read.
const REPO: &str = env!("CARGO_MANIFEST_DIR");

/// The contexts main's branch protection requires, as read when this was
/// written. The governance step reconciles the live settings against the
/// workflows; this holds the workflows to the list.
// llmlint: ignore[contracts_have_one_source_or_a_drift_gate] this is the fixed context list the workflows are held to, not a mirror of the live settings: those live in GitHub, which `just check` cannot read offline, and the create-repo skill's `setup_github_governance.py --verify` is the drift gate that reconciles them against the workflows whenever a job or leg is added or renamed.
const REQUIRED: &[&str] = &[
    "gate",
    "pr-title",
    "llmlint",
    "msrv",
    "deny",
    "wheel",
    "install (ubuntu-latest)",
    "install (macos-latest)",
    "install (windows-latest)",
    "cross (windows-latest)",
    "cross (macos-26)",
    "changes",
    "visual-docs / report (x86_64, shots/current, shots/verify, Visual docs)",
];

/// The repository a synthetic same-repository pull request comes from.
const REPOSITORY: &str = "nickderobertis/oneagentgraph";
/// The commit a synthetic push replaced.
const BEFORE: &str = "1111111111111111111111111111111111111111";

/// What a synthetic event sets, by expression path. Every path a scheduling
/// field reads must be here, with GitHub's own `null` written as [`Val::Null`]
/// where the event leaves it unset: a path missing from the map fails the
/// test rather than reading as null.
type Context = BTreeMap<String, Val>;

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Null,
    Bool(bool),
    Str(String),
}

impl Val {
    fn truthy(&self) -> bool {
        match self {
            Val::Null => false,
            Val::Bool(b) => *b,
            Val::Str(s) => !s.is_empty(),
        }
    }

    fn text(&self) -> String {
        match self {
            Val::Null => String::new(),
            Val::Bool(b) => b.to_string(),
            Val::Str(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Op(&'static str),
    Str(String),
    Name(String),
}

fn tokenize(expression: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = expression.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        let next = chars.get(at + 1).copied();
        if c.is_whitespace() {
            at += 1;
        } else if let Some(op) = [("||", "||"), ("&&", "&&"), ("==", "=="), ("!=", "!=")]
            .iter()
            .find(|(text, _)| c == text.chars().next().unwrap() && next == text.chars().nth(1))
        {
            tokens.push(Token::Op(op.1));
            at += 2;
        } else if let Some(op) = ["!", "(", ")", ","].iter().find(|op| op.starts_with(c)) {
            tokens.push(Token::Op(op));
            at += 1;
        } else if c == '\'' {
            let mut text = String::new();
            at += 1;
            loop {
                match (chars.get(at), chars.get(at + 1)) {
                    (Some('\''), Some('\'')) => {
                        text.push('\'');
                        at += 2;
                    }
                    (Some('\''), _) => {
                        at += 1;
                        break;
                    }
                    (Some(other), _) => {
                        text.push(*other);
                        at += 1;
                    }
                    (None, _) => panic!("unterminated string in: {expression}"),
                }
            }
            tokens.push(Token::Str(text));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = at;
            while at < chars.len()
                && (chars[at].is_ascii_alphanumeric() || matches!(chars[at], '_' | '.' | '-'))
            {
                at += 1;
            }
            tokens.push(Token::Name(chars[start..at].iter().collect()));
        } else {
            panic!("unreadable character {c:?} at {at} in: {expression}");
        }
    }
    tokens
}

struct Parser<'a> {
    tokens: Vec<Token>,
    at: usize,
    context: &'a Context,
    expression: &'a str,
}

impl Parser<'_> {
    fn peek_op(&self, op: &str) -> bool {
        matches!(self.tokens.get(self.at), Some(Token::Op(o)) if *o == op)
    }

    fn expect_op(&mut self, op: &str) {
        assert!(self.peek_op(op), "expected `{op}` in: {}", self.expression);
        self.at += 1;
    }

    fn or(&mut self) -> Val {
        let mut left = self.and();
        while self.peek_op("||") {
            self.at += 1;
            let right = self.and();
            left = if left.truthy() { left } else { right };
        }
        left
    }

    fn and(&mut self) -> Val {
        let mut left = self.equality();
        while self.peek_op("&&") {
            self.at += 1;
            let right = self.equality();
            left = if left.truthy() { right } else { left };
        }
        left
    }

    fn equality(&mut self) -> Val {
        let left = self.unary();
        for (op, wanted) in [("==", true), ("!=", false)] {
            if self.peek_op(op) {
                self.at += 1;
                let right = self.unary();
                // GitHub compares strings case-insensitively.
                let equal = left.text().to_lowercase() == right.text().to_lowercase()
                    && std::mem::discriminant(&left) == std::mem::discriminant(&right)
                    || (left == Val::Null && right == Val::Str(String::new()))
                    || (right == Val::Null && left == Val::Str(String::new()));
                return Val::Bool(equal == wanted);
            }
        }
        left
    }

    fn unary(&mut self) -> Val {
        if self.peek_op("!") {
            self.at += 1;
            return Val::Bool(!self.unary().truthy());
        }
        if self.peek_op("(") {
            self.at += 1;
            let value = self.or();
            self.expect_op(")");
            return value;
        }
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        match token {
            Some(Token::Str(text)) => Val::Str(text),
            Some(Token::Name(name)) if name == "true" => Val::Bool(true),
            Some(Token::Name(name)) if name == "false" => Val::Bool(false),
            Some(Token::Name(name)) if name == "null" => Val::Null,
            Some(Token::Name(name)) if self.peek_op("(") => self.call(&name),
            Some(Token::Name(path)) => self.context.get(&path).cloned().unwrap_or_else(|| {
                panic!(
                    "`{path}` has no value in this synthetic event, so `{}` cannot be \
                     evaluated; set what GitHub sets for it (`Val::Null` where it is unset) \
                     rather than letting it read as null",
                    self.expression
                )
            }),
            other => panic!("unexpected {other:?} in: {}", self.expression),
        }
    }

    fn call(&mut self, name: &str) -> Val {
        self.expect_op("(");
        let mut args = Vec::new();
        while !self.peek_op(")") {
            args.push(self.or());
            if self.peek_op(",") {
                self.at += 1;
            }
        }
        self.expect_op(")");
        let text = |i: usize| args[i].text().to_lowercase();
        match (name, args.len()) {
            ("startsWith", 2) => Val::Bool(text(0).starts_with(&text(1))),
            ("endsWith", 2) => Val::Bool(text(0).ends_with(&text(1))),
            ("contains", 2) => Val::Bool(text(0).contains(&text(1))),
            ("success" | "always", 0) => Val::Bool(true),
            ("failure" | "cancelled", 0) => Val::Bool(false),
            _ => panic!(
                "unknown function {name}/{} in: {}",
                args.len(),
                self.expression
            ),
        }
    }
}

/// Evaluate a field GitHub reads as an expression: an `if:` written with or
/// without `${{ }}`, or a value that is wholly one `${{ }}`.
fn evaluate(field: &str, context: &Context) -> Val {
    let trimmed = field.trim();
    let expression = trimmed
        .strip_prefix("${{")
        .and_then(|rest| rest.strip_suffix("}}"))
        .unwrap_or(trimmed);
    let mut parser = Parser {
        tokens: tokenize(expression),
        at: 0,
        context,
        expression,
    };
    let value = parser.or();
    assert_eq!(
        parser.at,
        parser.tokens.len(),
        "trailing tokens in: {expression}"
    );
    value
}

/// Evaluate a value field: literal text, or text that is wholly one expression.
fn render(field: &str, context: &Context) -> String {
    if field.trim().starts_with("${{") {
        evaluate(field, context).text()
    } else {
        field.to_string()
    }
}

struct Workflow {
    file: String,
    doc: Value,
}

fn workflows() -> Vec<Workflow> {
    let directory = Path::new(REPO).join(".github/workflows");
    let mut found: Vec<Workflow> = std::fs::read_dir(&directory)
        .expect("read .github/workflows")
        .map(|entry| entry.expect("a workflow directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == "yml" || ext == "yaml")
        })
        .map(|path| Workflow {
            file: path.file_name().unwrap().to_string_lossy().into_owned(),
            doc: serde_norway::from_str(&std::fs::read_to_string(&path).expect("read a workflow"))
                .unwrap_or_else(|err| panic!("{} is not YAML: {err}", path.display())),
        })
        .collect();
    found.sort_by(|a, b| a.file.cmp(&b.file));
    found
}

fn workflow(name: &str) -> Workflow {
    workflows()
        .into_iter()
        .find(|w| w.file == name)
        .unwrap_or_else(|| panic!("no .github/workflows/{name}"))
}

impl Workflow {
    fn jobs(&self) -> BTreeMap<String, &Value> {
        self.doc["jobs"]
            .as_mapping()
            .unwrap_or_else(|| panic!("{} declares no jobs", self.file))
            .iter()
            .map(|(id, job)| (id.as_str().expect("a job id").to_string(), job))
            .collect()
    }

    fn job(&self, id: &str) -> &Value {
        self.doc["jobs"]
            .get(id)
            .unwrap_or_else(|| panic!("{} has no `{id}` job", self.file))
    }

    /// Whether a pull request to main triggers this workflow on every push to it,
    /// with no path filter that could leave a required context unreported.
    fn runs_on_every_pull_request(&self) -> bool {
        let on = &self.doc["on"];
        let trigger = match on {
            Value::String(event) => return event == "pull_request",
            Value::Sequence(events) => return events.iter().any(|e| e == "pull_request"),
            _ => match on.get("pull_request") {
                Some(trigger) => trigger,
                None => return false,
            },
        };
        for filter in ["paths", "paths-ignore", "branches-ignore"] {
            assert!(
                trigger.get(filter).is_none(),
                "{}'s pull_request trigger carries `{filter}`, which can leave a required \
                 context unreported",
                self.file
            );
        }
        let listed = |key: &str, wanted: &str| {
            trigger
                .get(key)
                .and_then(Value::as_sequence)
                .is_none_or(|values| values.iter().any(|value| value == wanted))
        };
        listed("branches", "main") && listed("types", "synchronize")
    }

    /// The status-check contexts a job reports: its name, once per matrix leg.
    fn contexts(&self, id: &str) -> Vec<String> {
        let job = self.job(id);
        let name = job["name"].as_str().unwrap_or(id).to_string();
        if job.get("uses").is_some() {
            // A reusable workflow's jobs report as `<caller> / <callee job>`; the
            // callee is another repository's, so only the prefix is ours.
            return vec![format!("{name} / ")];
        }
        match job
            .get("strategy")
            .and_then(|s| s.get("matrix"))
            .and_then(Value::as_mapping)
        {
            None => vec![name],
            Some(matrix) => {
                assert_eq!(
                    matrix.len(),
                    1,
                    "{}'s `{id}` matrix has more than one axis; extend this test to render it",
                    self.file
                );
                let (_, values) = matrix.iter().next().unwrap();
                values
                    .as_sequence()
                    .expect("a matrix axis is a list")
                    .iter()
                    .map(|v| format!("{name} ({})", v.as_str().expect("a matrix value")))
                    .collect()
            }
        }
    }

    fn is_matrix(&self, id: &str) -> bool {
        self.job(id)
            .get("strategy")
            .and_then(|s| s.get("matrix"))
            .is_some()
    }

    fn needs(&self, id: &str) -> Vec<String> {
        match self.job(id).get("needs") {
            None => Vec::new(),
            Some(Value::String(one)) => vec![one.clone()],
            Some(Value::Sequence(many)) => many
                .iter()
                .map(|n| n.as_str().expect("a needs entry").to_string())
                .collect(),
            Some(other) => panic!("unreadable needs {other:?}"),
        }
    }

    /// Whether the workflow's `on:` takes an event: the event is listed, and the
    /// branch it targets (a pull request's base, a push's ref) passes any
    /// `branches` filter. A filter this cannot evaluate against a synthetic
    /// event — a glob, a path or tag filter — fails the test rather than being
    /// read as either answer.
    fn triggered(&self, context: &Context) -> bool {
        let event = context["github.event_name"].text();
        let on = &self.doc["on"];
        let trigger = match on {
            Value::String(listed) => return *listed == event,
            Value::Sequence(listed) => return listed.iter().any(|e| *e == *event),
            Value::Mapping(_) => match on.get(event.as_str()) {
                Some(trigger) => trigger,
                None => return false,
            },
            other => panic!("{}'s `on` is unreadable: {other:?}", self.file),
        };
        let branch = if event == "pull_request" {
            context["github.base_ref"].text()
        } else {
            let reference = context["github.ref"].text();
            match reference.strip_prefix("refs/heads/") {
                Some(branch) => branch.to_string(),
                None => panic!("a synthetic {event} names no branch: {reference}"),
            }
        };
        let Some(filters) = trigger.as_mapping() else {
            assert!(
                trigger.is_null(),
                "{}'s `{event}` trigger is unreadable: {trigger:?}",
                self.file
            );
            return true;
        };
        let mut taken = true;
        for (key, value) in filters {
            let key = key.as_str().expect("a trigger filter name");
            let listed: Vec<&str> = value
                .as_sequence()
                .unwrap_or_else(|| panic!("{}'s `{event}.{key}` is not a list", self.file))
                .iter()
                .map(|v| v.as_str().expect("a trigger filter value"))
                .collect();
            match key {
                "branches" => {
                    assert!(
                        listed.iter().all(|b| !b.contains(['*', '?', '[', '!'])),
                        "{}'s `{event}.branches` carries a pattern this cannot evaluate: \
                         {listed:?}",
                        self.file
                    );
                    taken &= listed.contains(&branch.as_str());
                }
                // The synthetic pull request is a push to an open one.
                "types" if event == "pull_request" => taken &= listed.contains(&"synchronize"),
                _ => panic!(
                    "{}'s `{event}` trigger filters on `{key}`, which a synthetic event \
                     cannot evaluate",
                    self.file
                ),
            }
        }
        taken
    }

    /// Whether GitHub schedules a job for an event: the workflow's `on:` takes
    /// it, every job it needs is scheduled, and its own `if` holds.
    fn scheduled(&self, id: &str, context: &Context) -> bool {
        self.triggered(context)
            && self
                .needs(id)
                .iter()
                .all(|need| self.scheduled(need, context))
            && self.job(id)["if"]
                .as_str()
                .is_none_or(|cond| evaluate(cond, context).truthy())
    }

    /// Every job a job waits on, directly or through another.
    fn waits_on(&self, id: &str) -> BTreeSet<String> {
        let mut all = BTreeSet::new();
        for need in self.needs(id) {
            all.extend(self.waits_on(&need));
            all.insert(need);
        }
        all
    }

    /// The `run:` text of every step a job runs for an event.
    fn runs(&self, id: &str, context: &Context) -> Vec<String> {
        self.job(id)["steps"]
            .as_sequence()
            .expect("a job has steps")
            .iter()
            .filter(|step| {
                step["if"]
                    .as_str()
                    .is_none_or(|c| evaluate(c, context).truthy())
            })
            .filter_map(|step| step["run"].as_str().map(str::to_string))
            .collect()
    }

    /// An env value a step sees for an event, by the step's `run:` text.
    fn step_env(&self, id: &str, run: &str, var: &str, context: &Context) -> String {
        let step = self.job(id)["steps"]
            .as_sequence()
            .expect("a job has steps")
            .iter()
            .find(|step| step["run"].as_str().is_some_and(|r| r.contains(run)))
            .unwrap_or_else(|| panic!("{}'s `{id}` job runs no `{run}`", self.file));
        let value = step["env"][var]
            .as_str()
            .unwrap_or_else(|| panic!("the `{run}` step in `{id}` sets no {var}"));
        render(value, context)
    }
}

fn event(name: &str, head_ref: &str, head_repo: &str, rust_affected: bool) -> Context {
    let text = |value: &str| Val::Str(value.to_string());
    let mut context = Context::from([
        ("github.event_name".to_string(), text(name)),
        ("github.repository".to_string(), text(REPOSITORY)),
        (
            "needs.changes.outputs.rust".to_string(),
            text(&rust_affected.to_string()),
        ),
    ]);
    // What GitHub sets for each event, and what it leaves unset: a pull request
    // has no `before` and no head commit in its payload, and a push has no head
    // or base branch (GitHub sets both to the empty string) and no pull request.
    let fields = if name == "pull_request" {
        [
            ("github.head_ref", text(head_ref)),
            ("github.base_ref", text("main")),
            ("github.ref", text("refs/pull/1/merge")),
            (
                "github.event.pull_request.head.repo.full_name",
                text(head_repo),
            ),
            ("github.event.before", Val::Null),
            ("github.event.head_commit.message", Val::Null),
        ]
    } else {
        [
            ("github.head_ref", text("")),
            ("github.base_ref", text("")),
            ("github.ref", text(&format!("refs/heads/{head_ref}"))),
            ("github.event.pull_request.head.repo.full_name", Val::Null),
            ("github.event.before", text(BEFORE)),
            ("github.event.head_commit.message", text("a merged change")),
        ]
    };
    context.extend(fields.map(|(path, value)| (path.to_string(), value)));
    context
}

fn pull_request(rust_affected: bool) -> Context {
    event(
        "pull_request",
        "feature/some-change",
        REPOSITORY,
        rust_affected,
    )
}

fn release_pr(rust_affected: bool) -> Context {
    event(
        "pull_request",
        "release-plz-2026-10-06T00-00-00Z",
        REPOSITORY,
        rust_affected,
    )
}

fn fork_pr() -> Context {
    event("pull_request", "patch-1", "someone/oneagentgraph", true)
}

fn push(rust_affected: bool) -> Context {
    event("push", "main", "", rust_affected)
}

/// A push to a branch other than main, which ci.yml's trigger does not take.
fn push_elsewhere() -> Context {
    event("push", "feature/some-change", "", true)
}

fn every_pull_request() -> Vec<(&'static str, Context)> {
    vec![
        (
            "an ordinary pull request reaching a Rust project",
            pull_request(true),
        ),
        (
            "an ordinary pull request reaching no Rust project",
            pull_request(false),
        ),
        ("the release PR", release_pr(true)),
        ("a release PR reaching no Rust project", release_pr(false)),
        ("a fork's pull request", fork_pr()),
    ]
}

#[test]
fn every_required_context_is_reported_on_every_pull_request() {
    let all = workflows();
    for required in REQUIRED {
        let producers: Vec<(&Workflow, String)> = all
            .iter()
            .filter(|w| w.runs_on_every_pull_request())
            .flat_map(|w| {
                w.jobs()
                    .into_keys()
                    .filter(|id| {
                        w.contexts(id).iter().any(|context| {
                            context == required
                                || (context.ends_with(" / ")
                                    && required.starts_with(context.as_str()))
                        })
                    })
                    .map(move |id| (w, id))
            })
            .collect();
        assert_eq!(
            producers.len(),
            1,
            "required context `{required}` must come from exactly one job a pull request runs; \
             found {:?}",
            producers
                .iter()
                .map(|(w, id)| format!("{}:{id}", w.file))
                .collect::<Vec<_>>()
        );
        let (workflow, id) = &producers[0];
        for (what, context) in every_pull_request() {
            // A job GitHub skips still reports its one context, as skipped. A
            // matrix job skipped at job level reports only its bare name, so its
            // legs would never appear: those must always be scheduled.
            if workflow.is_matrix(id) {
                assert!(
                    workflow.scheduled(id, &context),
                    "{}:{id} is skipped at job level for {what}, so `{required}` is never \
                     reported",
                    workflow.file
                );
            }
        }
    }
}

#[test]
fn a_leg_with_nothing_to_prove_reports_on_ubuntu_and_one_with_work_runs_it() {
    let ci = workflow("ci.yml");
    for id in ["cross", "install"] {
        let legs = ci.contexts(id);
        let job = ci.job(id);
        let os_values: Vec<String> = job["strategy"]["matrix"]["os"]
            .as_sequence()
            .expect("an os axis")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        for os in &os_values {
            for rust_affected in [true, false] {
                let mut context = pull_request(rust_affected);
                context.insert("matrix.os".into(), Val::Str(os.clone()));
                let runner = render(job["runs-on"].as_str().expect("a runs-on"), &context);
                let runs = ci.runs(id, &context);
                if rust_affected {
                    assert_eq!(&runner, os, "{id} ({os}) with work to do left its platform");
                    assert!(
                        runs.iter().all(|run| !run.contains("Nothing to prove")
                            && !run.contains("has nothing to")),
                        "{id} ({os}) ran its no-op step beside the real ones: {runs:?}"
                    );
                    assert!(runs.len() > 1, "{id} ({os}) ran no real step: {runs:?}");
                } else {
                    assert_eq!(
                        runner, "ubuntu-latest",
                        "{id} ({os}) waited for its platform to do nothing"
                    );
                    assert_eq!(
                        runs.len(),
                        1,
                        "{id} ({os}) with nothing to prove ran {runs:?}"
                    );
                }
            }
        }
        assert_eq!(legs.len(), os_values.len());
    }
}

#[test]
fn the_release_pr_runs_the_full_sweep_and_every_other_build_the_affected_tier() {
    let ci = workflow("ci.yml");

    // The sweep: release-plz's release PR, and nothing else.
    assert!(
        ci.scheduled("sweep", &release_pr(true)),
        "the release PR skipped the sweep"
    );
    for (what, context) in [
        ("an ordinary pull request", pull_request(true)),
        ("a fork's pull request", fork_pr()),
        ("a push to main", push(true)),
    ] {
        assert!(
            !ci.scheduled("sweep", &context),
            "{what} ran the full sweep"
        );
    }
    let sweep = ci.runs("sweep", &release_pr(true));
    assert!(
        sweep.iter().any(|run| run.trim() == "just check"),
        "the sweep job does not run the full `just check`: {sweep:?}"
    );

    // A push anywhere but main is not a build ci.yml takes at all.
    for id in ["gate", "sweep"] {
        assert!(
            !ci.scheduled(id, &push_elsewhere()),
            "a push to a feature branch scheduled `{id}`"
        );
    }

    // The gate: the affected tier, for every build it runs on, never the sweep.
    for (what, context) in [
        ("an ordinary pull request", pull_request(true)),
        ("the release PR", release_pr(true)),
        ("a push to main", push(true)),
    ] {
        assert!(ci.scheduled("gate", &context), "{what} skipped the gate");
        let runs = ci.runs("gate", &context);
        assert!(
            runs.iter().any(|run| run.trim() == "just check-affected"),
            "{what}'s gate does not run the affected tier: {runs:?}"
        );
        assert!(
            !runs.iter().any(|run| run.trim() == "just check"),
            "{what}'s gate runs the full sweep: {runs:?}"
        );
    }
}

#[test]
fn a_push_to_main_keys_its_affected_tier_to_the_commit_it_replaced() {
    let ci = workflow("ci.yml");
    for (id, run) in [
        ("gate", "just check-affected"),
        ("changes", "nx-affected.sh --affects oneagentgraph"),
    ] {
        assert_eq!(
            ci.step_env(id, run, "ONEAGENTGRAPH_NX_BASE_SHA", &push(true)),
            BEFORE,
            "a push's `{id}` does not hand nx-affected.sh the commit the push replaced"
        );
        // A pull request names no commit, so the script derives the merge base.
        assert_eq!(
            ci.step_env(id, run, "ONEAGENTGRAPH_NX_BASE_SHA", &pull_request(true)),
            "",
            "a pull request's `{id}` names a base commit instead of its merge base"
        );
    }
}

#[test]
fn the_unrequired_jobs_stay_out_of_the_required_set() {
    let all = workflows();
    let unrequired = [("notignored.yml", "suppressions"), ("ci.yml", "sweep")];
    for (file, id) in unrequired {
        let w = all.iter().find(|w| w.file == file).unwrap();
        for context in w.contexts(id) {
            assert!(
                !REQUIRED.contains(&context.as_str()),
                "{file}:{id} reports `{context}`, a required context"
            );
        }
    }
    // Nothing that reports a required context waits on either of them.
    for w in all.iter().filter(|w| w.runs_on_every_pull_request()) {
        for id in w.jobs().into_keys() {
            let reports_required = w
                .contexts(&id)
                .iter()
                .any(|c| REQUIRED.iter().any(|r| r == c || r.starts_with(c.as_str())));
            if !reports_required {
                continue;
            }
            for (file, unrequired_id) in unrequired {
                assert!(
                    !(w.file == file && w.waits_on(&id).contains(unrequired_id)),
                    "{}:{id} reports a required context and waits on {file}:{unrequired_id}",
                    w.file
                );
            }
        }
    }
}

#[test]
fn the_suppression_comment_runs_on_same_repository_pull_requests_only() {
    let w = workflow("notignored.yml");
    assert!(
        w.runs_on_every_pull_request(),
        "notignored.yml does not run on pull requests"
    );
    assert_eq!(w.doc["permissions"]["contents"], "read");
    assert_eq!(w.doc["permissions"]["pull-requests"], "write");
    let uses: Vec<&str> = w.job("suppressions")["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .filter_map(|step| step["uses"].as_str())
        .collect();
    assert!(
        uses.contains(&"nickderobertis/notignored@v0"),
        "the suppressions job does not run notignored: {uses:?}"
    );
    assert!(w.scheduled("suppressions", &pull_request(true)));
    assert!(
        !w.scheduled("suppressions", &fork_pr()),
        "a fork's pull request, whose token cannot comment, would run notignored"
    );
}

#[test]
fn a_release_is_gated_by_its_own_test_job_before_anything_publishes() {
    let release = workflow("release.yml");
    let runs = release.runs("test", &push(true));
    assert!(
        runs.iter().any(|run| run.trim() == "just check"),
        "release.yml's `test` job no longer runs `just check`: {runs:?}"
    );
    for id in ["publish-crate", "upload"] {
        assert!(
            release.waits_on(id).contains("test"),
            "release.yml's `{id}` can publish without waiting for `test`"
        );
    }
}

#[test]
#[should_panic(expected = "has no value in this synthetic event")]
fn an_expression_naming_a_value_the_event_does_not_set_fails_instead_of_reading_null() {
    // A misspelt context path must not quietly read as null, which would
    // schedule or skip a job by accident rather than by the workflow's logic.
    evaluate("github.head_reff == 'main'", &pull_request(true));
}
