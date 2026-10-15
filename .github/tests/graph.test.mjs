import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync, mkdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { execFileSync } from 'node:child_process';
import { workflows, resolveRun, triggered, digest, root } from '../workflow-graph.mjs';
const policyScript=resolve(root,'.github/macos-policy.mjs');
const required=['changes','cross (macos-26)','cross (windows-latest)','deny','gate','install (macos-latest)','install (ubuntu-latest)','install (windows-latest)','llmlint','msrv','pr-title','visual-docs / report (x86_64, shots/current, shots/verify, Visual docs)','wheel'];
const classes=[
 ['pr-opened','pull_request','opened'],['pr-synchronize','pull_request','synchronize'],['pr-reopened','pull_request','reopened'],
 ['base-push','push'],['release-merge','push'],['release-tag','push'],['release-published','release','published'],
 ['downstream','workflow_run','completed'],['repository-dispatch','repository_dispatch'],['schedule','schedule'],
 ['manual-default','workflow_dispatch'],['manual-opt-in','workflow_dispatch'],
];
function histories() {
 const dir=mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR??tmpdir(),'graph-'));
 const git=(...args)=>execFileSync('git',args,{cwd:dir,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
 git('init');git('config','user.email','test@example.com');git('config','user.name','Test');
 writeFileSync(join(dir,'Cargo.toml'),'[package]\nname="oneagentgraph"\nversion="0.1.0"\n[dependencies]\nx="1"\n');
 writeFileSync(join(dir,'CHANGELOG.md'),'# Changes\n');git('add','.');git('commit','-m','base');const base=git('rev-parse','HEAD');
 const heads={};
 for (const diff of ['source','version-only','dependency-content']) {
  git('checkout','-b',diff,base);
  if (diff==='source') writeFileSync(join(dir,'source.rs'),'fn main() {}');
  if (diff==='version-only') writeFileSync(join(dir,'Cargo.toml'),'[package]\nname="oneagentgraph"\nversion="0.2.0"\n[dependencies]\nx="1"\n');
  if (diff==='dependency-content') writeFileSync(join(dir,'Cargo.toml'),'[package]\nname="oneagentgraph"\nversion="0.2.0"\n[dependencies]\nx="2"\n');
  git('add','.');git('commit','-m',diff);heads[diff]=git('rev-parse','HEAD');
 }
 return {dir,base,heads};
}
const policyCache = new Map();
function classify(history, scenario) {
 const key=JSON.stringify([history.base,scenario.eventName,scenario.event,scenario.head]);
 if(policyCache.has(key)) return policyCache.get(key);
 const path=join(history.dir,'event.json');writeFileSync(path,JSON.stringify(scenario.event));
 const result = JSON.parse(execFileSync('node',[policyScript,'classify','--event',path,'--base',history.base,'--head',scenario.head],{cwd:history.dir,encoding:'utf8',env:{...process.env,GITHUB_OUTPUT:'',GITHUB_EVENT_NAME:scenario.eventName}}));
 policyCache.set(key,result);return result;
}
function scenario(history,id,eventName,action,diff,fork,releaseBranch,pypi,npm) {
 const repo='nickderobertis/oneagentgraph';
 return {id:`${id}/${diff}/${fork?'fork':'same'}/${releaseBranch?'release':'ordinary'}/${pypi}-${npm}`,class:id,eventName,action,diff,fork,releaseBranch,pypi,npm,head:history.heads[diff],ref:id==='release-tag'||eventName==='release'?'refs/tags/v0.2.0':eventName==='pull_request'?'refs/pull/1/merge':'refs/heads/main',upstream:'Release',event:{action,repository:{full_name:repo},...(eventName==='pull_request'?{pull_request:{head:{ref:releaseBranch?'release-plz-2026':'feature',repo:{full_name:fork?'someone/fork':repo}},base:{repo:{full_name:repo}},user:{login:'github-actions[bot]'},title:'chore: release'}}:{})},inputs:{macos:id==='manual-opt-in',version:''}};
}
function runs(docs,scenario,policy) {
 return docs.filter(w=>triggered(w.doc,scenario)).map(w=>({workflow:w.path,event:scenario.id,rows:resolveRun(w,scenario,policy,{inputs:scenario.inputs})}));
}
function fanout(docs,initial,scenario,policy) {
 const result=[...initial];
 // Follow workflow_run edges by the actual upstream workflow name, including
 // completion of smoke itself. Cycles cannot be silently discarded.
 function next(run,seen) {
  const upstream=docs.find(w=>w.path===run.workflow).doc.name;
  const child={...scenario,id:`${scenario.id} -> ${upstream}`,eventName:'workflow_run',action:'completed',upstream,ref:'refs/heads/main',event:{repository:scenario.event.repository,workflow_run:{head_sha:scenario.head,head_branch:'main',conclusion:'success'}},inputs:{}};
  for (const downstream of runs(docs,child,{isReleaseOnly:false,macosRequired:false,reason:'workflow completion'})) {
   if (seen.includes(downstream.workflow)) throw Error('workflow_run event cycle');
   result.push(downstream);next(downstream,[...seen,downstream.workflow]);
  }
 }
 for (const run of initial) next(run,[run.workflow]);
 return result;
}
test('actual graph: all event classes, semantic diffs, PR ownership and publish combinations',()=>{
 const history=histories();
 try {
  const docs=workflows(),scenarios=[],allRuns=[];
  for (const [id,eventName,action] of classes) for(const diff of Object.keys(history.heads)) for(const fork of [false,true]) for(const releaseBranch of [false,true]) for(const pypi of [false,true]) for(const npm of [false,true]) {
   const s=scenario(history,id,eventName,action,diff,fork,releaseBranch,pypi,npm);
   const policy=classify(history,s);
   assert.equal(policy.isReleaseOnly,eventName==='pull_request'&&releaseBranch&&!fork&&diff==='version-only',s.id);
   assert.equal(policy.macosRequired,eventName==='pull_request'&&!policy.isReleaseOnly,s.id);
   const resolved=runs(docs,s,policy);
   scenarios.push({...s,policy});allRuns.push(...resolved);
   const mac=resolved.flatMap(r=>r.rows).filter(r=>r.requested&&r.runner.startsWith('macos-'));
   if (eventName==='pull_request') {
    assert.deepEqual(mac.map(r=>`${r.workflow}:${r.job}`),policy.macosRequired?['.github/workflows/ci.yml:macos']:[],s.id);
    const names=resolved.flatMap(r=>r.rows).map(r=>r.name);
    for(const context of required) assert.equal(names.filter(n=>n===context).length,1,`${s.id}: ${context}`);
   } else if(eventName==='release') assert.deepEqual(mac.map(r=>r.job),['darwin'],s.id);
   else if(id==='manual-opt-in') assert.deepEqual(mac.map(r=>r.job),['macos-diagnostic'],s.id);
   else assert.deepEqual(mac,[],s.id);
  }
  for (const affected of [false,true]) {
   const s={...scenario(history,'pr-opened','pull_request','opened','source',false,false,false,false),affected};
   const rows=runs(docs,s,classify(history,s)).flatMap(run=>run.rows);
   assert.deepEqual(rows.filter(row=>row.requested&&row.runner.startsWith('macos-')).map(row=>row.job),affected?['macos']:[]);
   for(const row of rows.filter(row=>['cross','install'].includes(row.job))) if(!affected) assert.equal(row.runner,'ubuntu-latest');
  }
  const unknown={...scenario(history,'pr-opened','pull_request','opened','source',false,false,false,false),event:{}};
  const unknownPolicy=classify(history,unknown);
  assert.equal(unknownPolicy.macosRequired,true);
  assert.deepEqual(runs(docs,unknown,unknownPolicy).flatMap(run=>run.rows).filter(row=>row.requested&&row.runner.startsWith('macos-')).map(row=>row.job),['macos']);
  // Build complete one-push lifecycles from actual triggers and follow every
  // completion edge. A release without release-plz's PR is also supported.
  const lifecycles=[];
  for(const diff of Object.keys(history.heads)) for(const fork of [false,true]) for(const pypi of [false,true]) for(const npm of [false,true]) for(const releasePr of [false,true]) {
   const lifecycle=[];
   const events=[['pr-synchronize','pull_request','synchronize',diff,false],['base-push','push',undefined,diff,false],...(releasePr?[['pr-opened','pull_request','opened','version-only',true],['release-merge','push',undefined,'version-only',true]]:[]),['release-tag','push',undefined,'version-only',false],['release-published','release','published','version-only',false]];
   for(const [id,eventName,action,content,branch] of events) {
    const s=scenario(history,id,eventName,action,content,branch?false:fork,branch,pypi,npm);
    lifecycle.push(...fanout(docs,runs(docs,s,classify(history,s)),s));
    if (['base-push','release-merge'].includes(id)) {
     const closed={...scenario(history,`${id}-closed`,'pull_request','closed',content,branch?false:fork,branch,pypi,npm),ref:'refs/heads/main'};
     const closeRuns=runs(docs,closed,classify(history,closed));
     assert.ok(closeRuns.every(run=>run.workflow!=='.github/workflows/ci.yml'),'CI uses GitHub default PR actions, excluding closed');
     lifecycle.push(...fanout(docs,closeRuns,closed));
    }
   }
   lifecycles.push({diff,fork,pypi,npm,releasePr,runs:lifecycle});
  }
  mkdirSync(join(root,'.github/.telemetry'),{recursive:true});
  const telemetry={schema_version:2,sourceDigest:digest(),scenarios,runs:allRuns,lifecycles};
  const golden=JSON.parse(readFileSync(join(root,'.github/tests/fixtures/telemetry.v2.shape.json'),'utf8'));
  assert.equal(telemetry.schema_version,golden.schema_version);
  assert.deepEqual(Object.keys(telemetry),golden.rootKeys);
  for(const row of [...allRuns,...lifecycles.flatMap(lifecycle=>lifecycle.runs)].flatMap(run=>run.rows)) {
   assert.deepEqual(Object.keys(row).filter(key=>!golden.optionalRowKeys.includes(key)),golden.rowKeys);
   if(row.targets) assert.ok(row.targets.length>0,'empty optional target lists must be omitted');
  }
  assert.deepEqual(JSON.parse(JSON.stringify(telemetry)).runs,telemetry.runs,'job rows and optional targets must round-trip');
  writeFileSync(join(root,'.github/.telemetry/macos-policy.json'),JSON.stringify(telemetry,null,2)+'\n');
 } finally {rmSync(history.dir,{recursive:true,force:true});}
});
test('graph refuses unresolved runners, calls and matrices',()=>{
 const ci=workflows().find(w=>w.path.endsWith('/ci.yml'));
 const s={id:'negative',eventName:'pull_request',ref:'refs/pull/1/merge',event:{},inputs:{}};
 for(const mutation of [job=>job['runs-on']='${{ mystery.runner }}',job=>job.uses='other/repo/.github/workflows/run.yml@v1',job=>job.strategy={matrix:'${{ needs.unknown.outputs.matrix }}'}]) {
  const broken=structuredClone(ci);mutation(broken.doc.jobs.gate);
  assert.throws(()=>resolveRun(broken,s,{macosRequired:true,isReleaseOnly:false}),/unresolved|Unrecognized|unknown|Invalid|Unexpected/i);
 }
});
test('real carrier job remains scheduled with failed, cancelled or skipped prerequisites',()=>{
 const ci=workflows().find(w=>w.path.endsWith('/ci.yml'));
 const s={id:'results',eventName:'pull_request',ref:'refs/pull/1/merge',event:{},inputs:{}};
 for(const status of ['success','failure','cancelled','skipped']) for(const prerequisite of ['changes','gate','macos']) {
  const rows=resolveRun(ci,s,{macosRequired:true,isReleaseOnly:false},{overrides:{[prerequisite]:status}});
  const carriers=rows.filter(r=>r.job==='macos-results');assert.equal(carriers.length,2);assert.ok(carriers.every(r=>r.requested&&r.runner==='ubuntu-latest'));
 }
});

test('release artifact routes retain every platform, build Darwin arm64 alone, and gate it on local smokes',()=>{
 const release=workflows().find(w=>w.path.endsWith('/release.yml')).doc;
 const targets=Object.keys(JSON.parse(readFileSync(join(root,'npm/oneagentgraph-cli/package.json'))).optionalDependencies);
 const targetPackages=target=>`oneagentgraph-cli-${target.includes('apple')?'darwin':target.includes('windows')?'win32':'linux'}-${target.startsWith('aarch64')?'arm64':'x64'}`;
 const darwin=JSON.parse(release.jobs.darwin.env.DARWIN_TARGETS);
 assert.deepEqual(darwin,['aarch64-apple-darwin'],'the Mac release job builds Apple Silicon only');
 assert.ok(!targets.includes('oneagentgraph-cli-darwin-x64'),'no Intel macOS carrier is published');
 for(const job of ['upload','build-wheels','build-npm']) {
  const built=[...release.jobs[job].strategy.matrix.include.map(row=>row.target),...darwin];
  assert.deepEqual(built.map(targetPackages).sort(),[...targets].sort(),job);
 }
 const recorded=resolveRun({path:'.github/workflows/release.yml',doc:release},{id:'target-detail',eventName:'release',action:'published',ref:'refs/tags/v0.5.7',event:{}},{macosRequired:false,isReleaseOnly:false}).find(row=>row.job==='darwin');
 assert.deepEqual(recorded.targets,darwin,'telemetry must identify the Darwin target build');
 const steps=release.jobs.darwin.steps;
 const source=steps.filter(s=>s.run).map(s=>s.run).join('\n');
 for(const target of darwin) assert.ok(source.includes(`scripts/release-smoke.sh ${target}`));
 assert.match(source,/release-build\.sh/);assert.doesNotMatch(JSON.stringify(steps),/rosetta|x86_64|architecture":"x64/i,'native arm64 only: no Rosetta or Intel step');
 const firstUpload=steps.findIndex(s=>s.uses?.startsWith('actions/upload-artifact@'));
 assert.ok(steps.slice(0,firstUpload).filter(s=>s.run?.includes('scripts/release-smoke.sh')).length===darwin.length);
 const verifyAt=steps.findIndex(s=>s.run?.includes('scripts/release-verify.py'));
 assert.ok(verifyAt>steps.findIndex(s=>s.run?.includes('scripts/release-build.sh'))&&verifyAt<firstUpload,'the staged set is verified after it is built and before anything uploads');
 for(const publisher of ['publish-pypi','publish-npm','upload-darwin']) {
  assert.ok([].concat(release.jobs[publisher].needs).includes('darwin'),publisher);
  assert.equal(release.jobs[publisher]['runs-on'],'ubuntu-latest');
 }
 const artifacts=steps.filter(s=>s.uses?.startsWith('actions/upload-artifact@')).map(s=>s.with.name);
 for(const name of ['wheels-darwin','npm-darwin','archives-darwin']) assert.ok(artifacts.includes(name));
 // Default artifact transport is scoped to this exact workflow run/tag. A
 // download from another run or repository would evade pre-publication proofs.
 for(const job of Object.values(release.jobs)) for(const step of job.steps??[]) if(step.uses?.startsWith('actions/download-artifact@')) {
  assert.equal(step.with?.['run-id'],undefined);assert.equal(step.with?.repository,undefined);
 }
});

test('automatic callers cannot opt into the manual Mac diagnostic',()=>{
 const smoke=workflows().find(w=>w.path.endsWith('/published-smoke.yml'));
 for(const eventName of ['push','pull_request','release','workflow_run','repository_dispatch','schedule']) {
  const rows=resolveRun(smoke,{id:`injected-opt-in/${eventName}`,eventName,ref:'refs/heads/main',event:{}},{macosRequired:false,isReleaseOnly:false},{inputs:{macos:true,version:''}});
  assert.ok(rows.filter(row=>row.job==='macos-diagnostic').every(row=>!row.requested),eventName);
 }
});
