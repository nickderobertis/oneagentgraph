import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
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
  const staged = () => spawnSync('bash',['scripts/smoke-darwin.sh','x86_64-apple-darwin',dir,'1.2.3'],{encoding:'utf8'});
  const refuse = message => {const result=staged();assert.equal(result.status,2,result.stderr);assert.ok(result.stderr.includes(message),result.stderr);};
  refuse('missing staged archive');
  // Invalid input files are never consumed: each case stops at a missing or
  // ambiguous artifact, before tar, pip, npm or a platform executable runs.
  writeFileSync(join(dir,'oneagentgraph-v1.2.3-x86_64-apple-darwin.tar.gz'),'not consumed');
  refuse('missing staged npm carrier');
  writeFileSync(join(dir,'oneagentgraph-cli-darwin-x64-1.2.3.tgz'),'not consumed');
  refuse('stage exactly one');
  for(const version of ['1.2.3','1.2.4']) writeFileSync(join(dir,`oneagentgraph_cli-${version}-py3-none-macosx_10_12_x86_64.whl`),'not consumed');
  refuse('stage exactly one');
 } finally {rmSync(dir,{recursive:true,force:true});}
});
