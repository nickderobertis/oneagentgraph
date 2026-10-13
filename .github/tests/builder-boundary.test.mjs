import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
test('real Darwin command boundaries refuse bad inputs before compiler or installer work',()=>{
 const dir=mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR??tmpdir(),'darwin-boundary-'));
 try {
  for(const [script,args,reason] of [
   ['release-darwin',[],'three arguments'],['release-darwin',['unknown','v1.2.3',dir],'unsupported Darwin target'],
   ['release-darwin',['x86_64-apple-darwin','untrusted',dir],'release version'],
   ['release-darwin',['x86_64-apple-darwin','v1.2.3','relative'],'absolute'],
   ['smoke-darwin',[],'three arguments'],['smoke-darwin',['unknown',dir,'1.2.3'],'unsupported Darwin target'],
   ['smoke-darwin',['x86_64-apple-darwin','relative','1.2.3'],'absolute'],
   ['smoke-darwin',['x86_64-apple-darwin',dir,'../other'],'release-version syntax'],
  ]) {
   const result=spawnSync('bash',[`scripts/${script}.sh`,...args],{encoding:'utf8'});
   assert.equal(result.status,2,`${script}: ${result.stderr}`);
   assert.ok(result.stderr.includes(reason),result.stderr);assert.match(result.stderr,/ACTION: run bash scripts\//);
  }
 } finally {rmSync(dir,{recursive:true,force:true});}
});
