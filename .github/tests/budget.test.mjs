import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
test('reporters refuse missing or stale telemetry and never regenerate it',()=>{
 const dir=mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR??tmpdir(),'budget-'));
 try {
  for(const metric of ['jobs-per-run','runs-per-change']) {
   const run=path=>spawnSync('node',['.github/macos-budget.mjs','--metric',metric,'--telemetry',path],{encoding:'utf8'});
   const missing=run(join(dir,'missing.json'));assert.notEqual(missing.status,0);assert.match(missing.stderr,/ENOENT/);
   const path=join(dir,'stale.json');writeFileSync(path,JSON.stringify({schema_version:1,sourceDigest:'different-tree',scenarios:[{}],runs:[{}],lifecycles:[{}]}));
   const stale=run(path);assert.notEqual(stale.status,0);assert.match(stale.stderr,/stale macOS telemetry/);
  }
 } finally {rmSync(dir,{recursive:true,force:true});}
});
