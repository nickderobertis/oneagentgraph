import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { parse } from 'yaml';
test('Linux carriers cannot turn failed or missing work green', () => {
  const workflow = parse(readFileSync('.github/workflows/ci.yml', 'utf8'));
  const command = workflow.jobs['macos-results'].steps.find(step => step.env?.RESULTS).run;
  const run = overrides => {
    const values = {required:'true',affected:'true',changes:'success',gate:'success',macos:'success',...overrides};
    const needs = { changes:{result:values.changes,outputs:{macosRequired:values.required,rust:values.affected}},gate:{result:values.gate},macos:{result:values.macos} };
    return spawnSync('bash', ['-c', command], {encoding:'utf8',env:{...process.env,RESULTS:JSON.stringify(needs)}});
  };
  assert.equal(run({}).status, 0);
  for (const status of ['failure', 'cancelled', 'skipped', undefined]) {
    assert.notEqual(run({ macos: status }).status, 0);
    assert.notEqual(run({ changes: status }).status, 0);
    assert.notEqual(run({ gate: status }).status, 0);
  }
  for (const omit of [{ required: 'false' }, { affected: 'false' }]) {
    const result = run({ ...omit, macos: 'skipped' });
    assert.equal(result.status, 0); assert.match(result.stdout, /did not run/);
    for (const status of ['success', 'failure', 'cancelled', undefined]) assert.notEqual(run({ ...omit, macos: status }).status, 0);
  }
  assert.notEqual(run({ required: undefined }).status, 0);
});
