import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
test('Linux carriers cannot turn failed or missing work green', () => {
  const run = changes => spawnSync('node', ['.github/macos-result.mjs', JSON.stringify({ required: 'true', affected: 'true', changes: 'success', gate: 'success', macos: 'success', ...changes })], { encoding: 'utf8' });
  assert.equal(run({}).status, 0);
  for (const status of ['failure', 'cancelled', 'skipped', undefined]) {
    assert.notEqual(run({ macos: status }).status, 0);
    assert.notEqual(run({ changes: status }).status, 0);
    assert.notEqual(run({ gate: status }).status, 0);
  }
  for (const omit of [{ required: 'false' }, { affected: 'false' }]) {
    const result = run({ ...omit, macos: 'skipped' });
    assert.equal(result.status, 0); assert.match(result.stdout, /did not run/);
    assert.notEqual(run({ ...omit, macos: undefined }).status, 0);
  }
  assert.notEqual(run({ required: undefined }).status, 0);
});
