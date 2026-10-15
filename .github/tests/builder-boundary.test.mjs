import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
test('real release command boundaries refuse bad inputs before compiler or installer work',()=>{
 const dir=mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR??tmpdir(),'darwin-boundary-'));
 try {
  for(const [script,args,reason] of [
   ['release-build',[],'three arguments'],['release-build',['x86_64-apple-darwin','v1.2.3',dir],'Intel macOS is no longer a release target'],['release-build',['unknown','v1.2.3',dir],'unsupported release target'],
   ['release-build',['aarch64-apple-darwin','untrusted',dir],'release version'],
   ['release-build',['aarch64-apple-darwin','v1.2.3','relative'],'absolute'],
   ['release-smoke',[],'three arguments'],['release-smoke',['x86_64-apple-darwin',dir,'1.2.3'],'Intel macOS is no longer a release target'],['release-smoke',['unknown',dir,'1.2.3'],'unsupported release target'],
   ['release-smoke',['aarch64-apple-darwin','relative','1.2.3'],'absolute'],
   ['release-smoke',['aarch64-apple-darwin',dir,'../other'],'release-version syntax'],
  ]) {
   const result=spawnSync('bash',[`scripts/${script}.sh`,...args],{encoding:'utf8'});
   assert.equal(result.status,2,`${script}: ${result.stderr}`);
   assert.ok(result.stderr.includes(reason),result.stderr);assert.match(result.stderr,/ACTION: run bash scripts\//);
  }
  const staged = () => spawnSync('bash',['scripts/release-smoke.sh','aarch64-apple-darwin',dir,'1.2.3'],{encoding:'utf8'});
  const refuse = message => {const result=staged();assert.equal(result.status,2,result.stderr);assert.ok(result.stderr.includes(message),result.stderr);};
  refuse('missing staged archive');
  // Invalid input files are never consumed: each case stops at a missing or
  // ambiguous artifact, before tar, pip, npm or a platform executable runs.
  writeFileSync(join(dir,'oneagentgraph-v1.2.3-aarch64-apple-darwin.tar.gz'),'not consumed');
  refuse('missing staged npm carrier');
  writeFileSync(join(dir,'oneagentgraph-cli-darwin-arm64-1.2.3.tgz'),'not consumed');
  refuse('stage exactly one');
  for(const version of ['1.2.3','1.2.4']) writeFileSync(join(dir,`oneagentgraph_cli-${version}-py3-none-macosx_11_0_arm64.whl`),'not consumed');
  refuse('stage exactly one');
 } finally {rmSync(dir,{recursive:true,force:true});}
});

test('real packager refuses the discontinued Intel macOS target',()=>{
 const result=spawnSync('python3',['scripts/release-package.py','--target','x86_64-apple-darwin','--binary','not-read','--tag','v1.2.3','--out','/nonexistent-never-written'],{encoding:'utf8'});
 assert.equal(result.status,2,result.stderr);
 assert.match(result.stderr,/invalid choice: 'x86_64-apple-darwin'/);
});

test('real packager rejects an invalid output path before touching artifacts',()=>{
 const result=spawnSync('python3',['scripts/release-package.py','--target','aarch64-apple-darwin','--binary','not-read','--tag','v1.2.3','--out','relative-output'],{encoding:'utf8'});
 assert.equal(result.status,2,result.stderr);
 assert.match(result.stderr,/--out must be an absolute artifact directory/);
});

test('real attach script refuses bad input before gh is reached',()=>{
 const dir=mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR??tmpdir(),'attach-boundary-'));
 try {
  const bin=join(dir,'bin'),staged=join(dir,'staged'),record=join(dir,'gh-called');
  mkdirSync(bin);mkdirSync(staged);
  writeFileSync(join(bin,'gh'),`#!/bin/sh\necho "$@" >> '${record}'\n`);chmodSync(join(bin,'gh'),0o755);
  for(const [args,reason] of [
   [[],'two arguments'],
   [['--repo=elsewhere',staged],'release tag must be a vX.Y.Z version'],
   [['v1.2.3',join(dir,'absent')],'staged artifact directory does not exist'],
   [['v1.2.3',staged],'no archive or checksum is staged'],
  ]) {
   const result=spawnSync('bash',['scripts/release-attach.sh',...args],{encoding:'utf8',env:{...process.env,PATH:`${bin}:${process.env.PATH}`}});
   assert.equal(result.status,2,result.stderr);
   assert.ok(result.stderr.includes(reason),result.stderr);assert.match(result.stderr,/ACTION: run bash scripts\/release-attach\.sh/);
  }
  assert.equal(existsSync(record),false,'gh ran for a refused attach');
 } finally {rmSync(dir,{recursive:true,force:true});}
});

test('real packager names the format and CPU of every binary it refuses',()=>{
 const dir=mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR??tmpdir(),'header-boundary-'));
 try {
  const version=readFileSync('Cargo.toml','utf8').match(/^version = "([^"]+)"/m)[1];
  const machO=cpu=>{const b=Buffer.alloc(32);b.writeUInt32LE(0xfeedfacf,0);b.writeUInt32LE(cpu,4);return b;};
  for(const [name,bytes,found] of [
   ['universal',Buffer.from([0xca,0xfe,0xba,0xbe,0,0,0,2]),'mach-o universal/multiple'],
   ['intel-mach-o',machO(0x01000007),'mach-o/x86_64'],
   ['unknown-mach-o',machO(0x12),'mach-o/cpu 0x12'],
   ['pe',Buffer.from('MZ\x90\x00'),'pe/?'],
   ['truncated-elf',Buffer.from('\x7fELF\x02\x01'),'unknown/?'],
   ['text',Buffer.from('#!/bin/sh\n'),'unknown/?'],
  ]) {
   writeFileSync(join(dir,name),bytes);
   const out=join(dir,`out-${name}`);
   const result=spawnSync('python3',['scripts/release-package.py','--target','aarch64-apple-darwin','--binary',join(dir,name),'--tag',`v${version}`,'--out',out],{encoding:'utf8'});
   assert.equal(result.status,2,`${name}: ${result.stderr}`);
   assert.ok(result.stderr.includes(`built for ${found}, not mach-o/arm64`),`${name}: ${result.stderr}`);
   assert.equal(existsSync(out),false,`${name}: the packager wrote output`);
  }
 } finally {rmSync(dir,{recursive:true,force:true});}
});

test('real verifier refuses a bad tag or staged directory before reading artifacts',()=>{
 for(const [args,reason] of [
  [['--tag','1.2.3','--dir',tmpdir()],'--tag must be a vX.Y.Z release tag'],
  [['--tag','v1.2.3','--dir','relative'],'--dir must be an existing absolute'],
  [['--tag','v1.2.3','--dir','/nonexistent-staged-directory'],'--dir must be an existing absolute'],
 ]) {
  const result=spawnSync('python3',['scripts/release-verify.py','--target','aarch64-apple-darwin',...args],{encoding:'utf8'});
  assert.equal(result.status,2,result.stderr);assert.ok(result.stderr.includes(reason),result.stderr);
 }
});
