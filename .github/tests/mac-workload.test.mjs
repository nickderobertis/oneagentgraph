import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { workflows, root } from '../workflow-graph.mjs';

const job = (file, id) => workflows().find(w => w.path.endsWith(`/${file}`)).doc.jobs[id];

// The Mac PR proof job as it stood before Intel macOS was dropped; it carried no
// Intel step, so removing Intel leaves it exactly this. Native release packaging
// belongs to release.yml's Darwin job, not to this job's workload.
const PR_PROOF = {
 needs: ['changes', 'gate'],
 if: "needs.changes.outputs.macosRequired == 'true' && needs.changes.outputs.rust == 'true'",
 'runs-on': 'macos-26',
 defaults: { run: { shell: 'bash' } },
 steps: [
  { uses: 'actions/checkout@v4' },
  { uses: 'Swatinem/rust-cache@v2' },
  { uses: 'actions/setup-node@v4', with: { 'node-version': '22' } },
  { uses: 'taiki-e/install-action@v2', with: { tool: 'just,nextest' } },
  { name: 'Bootstrap', run: 'just bootstrap' },
  { name: 'Format + lint', run: 'just fmt-check && just lint' },
  { name: 'Tests (unit + contract + e2e + repo tooling)', run: 'just test-quick' },
  { name: 'Install (this revision)', env: { RUSTFLAGS: '-D warnings' }, run: 'cargo install --path . --locked' },
  { name: 'Smoke-test the installed binary', run: 'bash scripts/smoke-published.sh --label "cargo install on $RUNNER_OS/$RUNNER_ARCH"' },
 ],
};

test('the Mac PR proof job keeps its workload', () => {
 assert.deepEqual(job('ci.yml', 'macos'), PR_PROOF);
});

test('bootstrap provisions no packaging toolchain on the Mac PR runner', () => {
 const dir = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), 'mac-bootstrap-'));
 try {
  const record = join(dir, 'invoked');
  for (const [name, body] of [['uname', 'echo Darwin'], ['cargo', `echo cargo "$@" >> '${record}'; exit 1`], ['maturin', `echo maturin "$@" >> '${record}'; exit 1`]]) {
   writeFileSync(join(dir, name), `#!/bin/sh\n${body}\n`);
   chmodSync(join(dir, name), 0o755);
  }
  const result = spawnSync('bash', ['scripts/setup-packaging.sh'], { cwd: root, encoding: 'utf8', env: { ...process.env, PATH: `${dir}:${process.env.PATH}`, CARGO_HOME: dir } });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(existsSync(record), false, 'macOS bootstrap must not install or probe maturin');
 } finally { rmSync(dir, { recursive: true, force: true }); }
});

test('the Mac release job runs the native packaging journey before anything publishes', () => {
 const darwin = job('release.yml', 'darwin');
 const steps = darwin.steps;
 const at = text => steps.findIndex(step => step.run?.includes(text));
 const [build, verify, rehearse, smoke] = ['scripts/release-build.sh', 'scripts/release-verify.py', 'scripts/release-rehearse.sh', 'scripts/release-smoke.sh'].map(at);
 const firstUpload = steps.findIndex(step => step.uses?.startsWith('actions/upload-artifact@'));
 assert.ok(build >= 0 && build < verify && verify < rehearse && rehearse < smoke && smoke < firstUpload, JSON.stringify({ build, verify, rehearse, smoke, firstUpload }));
 assert.match(steps[rehearse].run, /DARWIN_TARGETS/, 'every declared Darwin target is rehearsed');
 assert.deepEqual(JSON.parse(darwin.env.DARWIN_TARGETS), ['aarch64-apple-darwin']);
 // No publisher runs before the Mac job, so none runs before its rehearsal.
 for (const publisher of ['publish-crate', 'upload', 'publish-pypi', 'publish-npm', 'upload-darwin']) assert.ok([].concat(job('release.yml', publisher).needs).includes('darwin'), publisher);
 // The rehearsal doubles only the transport of the scripts the publishers run.
 const rehearsal = readFileSync(join(root, 'scripts/release-rehearse.sh'), 'utf8');
 assert.match(job('release.yml', 'upload-darwin').steps.at(-1).run, /^bash scripts\/release-attach\.sh /);
 assert.ok(job('release.yml', 'publish-npm').steps.some(step => step.run?.includes('bash scripts/publish-npm.sh "$tgz"')));
 for (const script of ['scripts/release-attach.sh', 'scripts/publish-npm.sh']) assert.ok(rehearsal.includes(`"$root/${script}"`), script);
 const macJobs = Object.entries(workflows().find(w => w.path.endsWith('/release.yml')).doc.jobs).filter(([, spec]) => String(spec['runs-on']).startsWith('macos-')).map(([id]) => id);
 assert.deepEqual(macJobs, ['darwin'], 'the journey adds no macOS job');
});
