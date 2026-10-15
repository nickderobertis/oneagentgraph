import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { execFileSync } from 'node:child_process';
const script = resolve('.github/macos-policy.mjs');
test('real classifier over git histories: release identity alone never authorizes omission', () => {
  const dir = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), 'policy-'));
  const git = (...args) => execFileSync('git', args, { cwd: dir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  try {
    git('init'); git('config', 'user.email', 'test@example.com'); git('config', 'user.name', 'Test');
    writeFileSync(join(dir, 'Cargo.toml'), '[package]\nname="oneagentgraph"\nversion="0.1.0"\n[dependencies]\nx="1"\n');
    writeFileSync(join(dir, 'CHANGELOG.md'), '# Changes\n');
    const lock = version => `version = 4\n[[package]]\nname = "oneagentgraph"\nversion = "0.1.0"\n[[package]]\nname = "oneagentgraph"\nversion = "${version}"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "abc"\n`;
    writeFileSync(join(dir, 'Cargo.lock'), lock('0.1.0'));
    git('add', '.'); git('commit', '-m', 'base'); const base = git('rev-parse', 'HEAD');
    const event = { repository: { full_name: 'nickderobertis/oneagentgraph' }, pull_request: { title: 'chore: release', user: { login: 'github-actions[bot]' }, head: { ref: 'release-plz-2026', repo: { full_name: 'nickderobertis/oneagentgraph' } }, base: { repo: { full_name: 'nickderobertis/oneagentgraph' } } } };
    const run = (input = event, eventName = 'pull_request', head = 'HEAD') => {
      writeFileSync(join(dir, 'event.json'), typeof input === 'string' ? input : JSON.stringify(input));
      return JSON.parse(execFileSync('node', [script, 'classify', '--event', join(dir, 'event.json'), '--base', base, '--head', head], { cwd: dir, encoding: 'utf8', env: { ...process.env, GITHUB_EVENT_NAME: eventName, GITHUB_OUTPUT: '' } }));
    };
    writeFileSync(join(dir, 'Cargo.toml'), '[package]\nname="oneagentgraph"\nversion="0.2.0"\n[dependencies]\nx="1"\n');
    git('add', 'Cargo.toml'); git('commit', '-m', 'version');
    assert.equal(run().isReleaseOnly, true);
    const unknown = run(event, 'unknown-ci-event');
    assert.equal(unknown.macosRequired, true);
    assert.match(unknown.reason, /unknown-ci-event/);
    const human = structuredClone(event); human.pull_request.user.login = 'nickderobertis';
    assert.equal(run(human).isReleaseOnly, true);
    const forged = structuredClone(event); forged.pull_request.head.ref = 'feature';
    assert.equal(run(forged).macosRequired, true);
    const fork = structuredClone(event); fork.pull_request.head.repo.full_name = 'fork/repo';
    assert.equal(run(fork).macosRequired, true);
    assert.equal(run('{').macosRequired, true);
    assert.equal(run(event, 'pull_request', 'missing-ref').macosRequired, true);
    for (const name of ['push', 'release', 'workflow_run', 'schedule', 'repository_dispatch', 'workflow_dispatch']) assert.equal(run('{', name).macosRequired, false);
    writeFileSync(join(dir, 'Cargo.lock'), lock('0.2.0'));
    git('add', 'Cargo.lock'); git('commit', '-m', 'same-named registry dependency');
    assert.equal(run().macosRequired, true);
    writeFileSync(join(dir, 'Cargo.lock'), lock('0.1.0'));
    writeFileSync(join(dir, 'Cargo.toml'), '[package]\nname="oneagentgraph"\nversion="0.2.0"\n[dependencies]\nx="2"\n');
    git('add', 'Cargo.toml'); git('commit', '-m', 'dependency');
    assert.equal(run().macosRequired, true);
    writeFileSync(join(dir, 'source.rs'), 'fn main() {}'); git('add', 'source.rs'); git('commit', '-m', 'source');
    assert.equal(run().macosRequired, true);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('real classifier over the launcher manifest: only its version pins may move', () => {
  const dir = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), 'policy-launcher-'));
  const git = (...args) => execFileSync('git', args, { cwd: dir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
  // This repository's own launcher manifest, so the platform set under test is
  // the one that ships.
  const shipped = JSON.parse(readFileSync(resolve('npm/oneagentgraph-cli/package.json'), 'utf8'));
  const launcher = (version, change = deps => deps) => JSON.stringify({ ...shipped, version, optionalDependencies: change(Object.fromEntries(Object.keys(shipped.optionalDependencies).map(name => [name, version]))) }, null, 2);
  const path = join(dir, 'npm/oneagentgraph-cli/package.json');
  try {
    git('init'); git('config', 'user.email', 'test@example.com'); git('config', 'user.name', 'Test');
    mkdirSync(join(dir, 'npm/oneagentgraph-cli'), { recursive: true });
    writeFileSync(path, launcher('0.1.0'));
    git('add', '.'); git('commit', '-m', 'base'); const base = git('rev-parse', 'HEAD');
    const event = { repository: { full_name: 'nickderobertis/oneagentgraph' }, pull_request: { title: 'chore: release', user: { login: 'github-actions[bot]' }, head: { ref: 'release-plz-2026', repo: { full_name: 'nickderobertis/oneagentgraph' } }, base: { repo: { full_name: 'nickderobertis/oneagentgraph' } } } };
    writeFileSync(join(dir, 'event.json'), JSON.stringify(event));
    const classify = () => JSON.parse(execFileSync('node', [script, 'classify', '--event', join(dir, 'event.json'), '--base', base, '--head', 'HEAD'], { cwd: dir, encoding: 'utf8', env: { ...process.env, GITHUB_EVENT_NAME: 'pull_request', GITHUB_OUTPUT: '' } }));
    const commit = (text, message) => { writeFileSync(path, text); git('add', path); git('commit', '-m', message); return classify(); };
    const bumped = commit(launcher('0.2.0'), 'bump'); assert.equal(bumped.isReleaseOnly, true, bumped.reason);
    assert.equal(commit(launcher('0.2.0', deps => ({ ...deps, 'oneagentgraph-cli-darwin-x64': '0.2.0' })), 'add a platform').macosRequired, true);
    const [first] = Object.keys(shipped.optionalDependencies);
    assert.equal(commit(launcher('0.2.0', deps => { const { [first]: pin, ...rest } = deps; return { ...rest, [`${first}-renamed`]: pin }; }), 'rename a platform').macosRequired, true);
    assert.equal(commit(launcher('0.2.0', deps => ({ ...deps, [first]: 'file:../elsewhere' })), 'a non-version pin').macosRequired, true);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
