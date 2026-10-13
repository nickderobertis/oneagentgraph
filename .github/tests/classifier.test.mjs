import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
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
