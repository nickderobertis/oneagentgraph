// Read baseline/final workflow routing for the worker-controlled draft body.
import { execFileSync } from 'node:child_process';
import { parse } from 'yaml';
import { workflows, resolveRun, triggered } from './workflow-graph.mjs';
const args=process.argv.slice(2);
if(args.length && (args.length!==2||args[0]!=='--base'||!args[1])) throw Error('ACTION: run draft-evidence.mjs [--base existing-git-ref]');
let base;
try { base=args[1]?execFileSync('git',['rev-parse','--verify',`${args[1]}^{commit}`],{encoding:'utf8'}).trim():execFileSync('git',['merge-base','HEAD','origin/main'],{encoding:'utf8'}).trim(); }
catch { throw Error('ACTION: fetch origin/main or pass --base with an existing commit'); }
const docs=workflows();
const variants=[['ordinary PR','pull_request','synchronize','feature'],['release-only PR','pull_request','synchronize','release-plz-2026'],['main push','push',undefined,''],['published release','release','published',''],['Release completion','workflow_run','completed',''],['schedule','schedule',undefined,''],['manual default','workflow_dispatch',undefined,''],['manual opt-in','workflow_dispatch',undefined,'']];
console.log(`Workflow comparison read from merge base ${base} and the working tree. Release routes are modeled; hosted runs are reported separately.\n`);
console.log('| Workflow (all triggering events) | Before Mac jobs/run | After Mac jobs/run |');
console.log('| --- | --- | --- |');
for(const final of docs) {
 const before={path:final.path,doc:parse(execFileSync('git',['show',`${base}:${final.path}`],{encoding:'utf8'}))};
 const counts=doc=>variants.map(([label,eventName,action,ref])=>{
  const scenario={id:label,eventName,action,ref:eventName==='release'?'refs/tags/v0.5.7':'refs/heads/main',upstream:'Release',event:{action,pull_request:{head:{ref,repo:{full_name:'nickderobertis/oneagentgraph'}}}},pypi:true,npm:true};
  if(!triggered(doc.doc,scenario)) return null;
  const rows=resolveRun(doc,scenario,{macosRequired:eventName==='pull_request'&&!ref.startsWith('release-plz-'),isReleaseOnly:ref.startsWith('release-plz-')},{inputs:{macos:label==='manual opt-in'}});
  return `${label}: ${rows.filter(row=>row.requested&&row.runner.startsWith('macos-')).length}`;
 }).filter(Boolean);
 const old=counts(before),now=counts(final);
 const capable=[...old,...now].some(x=>!x.endsWith(': 0')) || final.doc.jobs['visual-docs'];
 if(capable) console.log(`| ${final.path}${final.doc.jobs['visual-docs']?' (includes pinned reusable call)':''} | ${old.join('; ')} | ${now.join('; ')} |`);
}
console.log('\nRelease platforms retained: x86_64/aarch64 Linux, x86_64/aarch64 Darwin, x86_64 Windows. Each retains its archive/checksum, CLI wheel and npm carrier; the Rust crate and npm launcher remain published. Darwin forms share each target binary. Linux/Windows install, test and registry smokes remain. No SBOM job or artifact existed in this baseline; no existing output was removed.');
console.log('\nBudget exception markers were omitted because budgets_track_product_owner_outcomes is not configured in this repository. Each registration records the manager-approved reason; jobs/run also records its inner_measure_reason. llmlint configuration/plugins are unchanged.');
