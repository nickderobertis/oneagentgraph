import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdtempSync, existsSync, mkdirSync, copyFileSync, symlinkSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { parse, stringify } from 'yaml';
import { root, workflows } from '../workflow-graph.mjs';

test('evidence command renders the actual workflow table and diagnoses an invalid base',()=>{
 const result=spawnSync('node',['.github/draft-evidence.mjs','--base','HEAD'],{cwd:root,encoding:'utf8'});
 assert.equal(result.status,0,result.stderr);
 assert.match(result.stdout,/Before Mac jobs\/run.*After Mac jobs\/run/);
 for(const workflow of workflows().filter(w=>JSON.stringify(w.doc).includes('macos-')||Object.values(w.doc.jobs).some(j=>j.uses))) assert.ok(result.stdout.includes(workflow.path),workflow.path);
 for(const event of ['ordinary PR','release-only PR','main push','published release','Release completion','schedule','manual default','manual opt-in']) assert.ok(result.stdout.includes(event),event);
 assert.match(result.stdout,/modeled; hosted runs are reported separately/);
 const invalid=spawnSync('node',['.github/draft-evidence.mjs','--base','nonexistent-ref'],{cwd:root,encoding:'utf8'});
 assert.notEqual(invalid.status,0);assert.match(invalid.stderr,/ACTION: fetch origin\/main/);
});

test('real reporters and SDK checker emit measurements and propagate a refused budget',()=>{
 const dir=mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR??tmpdir(),'reporting-'));
 const run=(command,args,env={})=>{const environment={...process.env,...env};delete environment.NODE_TEST_CONTEXT;return spawnSync(command,args,{cwd:dir,encoding:'utf8',env:environment});};
 try {
  // A real checkout around the real commands. No evaluator, telemetry producer,
  // measurement reporter, SDK, or onebudgetspec binary is replaced.
  const paths=new Set(execFileSync('git',['ls-files','--cached','--others','--exclude-standard'],{cwd:root,encoding:'utf8'}).trim().split('\n'));
  for(const path of paths) {if(!existsSync(join(root,path))) continue;mkdirSync(dirname(join(dir,path)),{recursive:true});copyFileSync(join(root,path),join(dir,path));}
  symlinkSync(join(root,'node_modules'),join(dir,'node_modules'),'dir');
  symlinkSync(join(root,'.github/node_modules'),join(dir,'.github/node_modules'),'dir');
  assert.equal(run('git',['init']).status,0);
  // Directory-only ignore rules do not match these fixture dependency symlinks.
  writeFileSync(join(dir,'.git/info/exclude'),'/node_modules\n/.github/node_modules\n');
  const produce=()=>{
   const result=run('node',['--test','.github/tests/graph.test.mjs']);
   assert.equal(result.status,0,result.stdout+result.stderr);
  };
  produce();
  const registration=parse(readFileSync(join(dir,'.github/budgets.yaml'),'utf8'));
  for(const budget of registration.budgets) {
   const metric=budget.command[budget.command.indexOf('--metric')+1];
   const reported=join(dir,`.github/.telemetry/${metric}.result.json`);
   const result=run('node',['.github/macos-budget.mjs','--metric',metric,'--telemetry','.github/.telemetry/macos-policy.json'],{ONEBUDGETSPEC_RESULT:reported});
   assert.equal(result.status,0,result.stderr);
   const summary=JSON.parse(result.stdout),measurement=JSON.parse(readFileSync(reported,'utf8'));
   assert.equal(summary.metric,metric);assert.equal(summary.value,measurement.value);
   const detail=JSON.parse(measurement.detail);
   assert.ok(detail.worst.length>0);assert.match(measurement.detail,/x86_64-apple-darwin/);assert.match(measurement.detail,/aarch64-apple-darwin/);
  }
  const accepted=run('node',['.github/check-budgets.mjs']);
  assert.equal(accepted.status,0,accepted.stderr);
  assert.deepEqual(JSON.parse(accepted.stdout).map(result=>result.id),registration.budgets.map(budget=>budget.id));
  assert.ok(JSON.parse(accepted.stdout).every(result=>result.verdict==='within'));
  // Only the SDK judges thresholds. This intentionally impossible registration
  // is confined to this disposable checkout to drive the checker's exit path.
  for(const budget of registration.budgets) budget.threshold=0;
  writeFileSync(join(dir,'.github/budgets.yaml'),stringify(registration));
  produce();
  const refused=run('node',['.github/check-budgets.mjs']);
  assert.equal(refused.status,1,refused.stderr);
  assert.ok(JSON.parse(refused.stdout).every(result=>result.verdict==='over'));
 } finally {rmSync(dir,{recursive:true,force:true});}
});
