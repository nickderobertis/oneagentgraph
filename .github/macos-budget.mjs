import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { report } from '@onebudgetspec/sdk';
import { digest, root } from './workflow-graph.mjs';
const args=process.argv.slice(2);
if(args.length!==4||args[0]!=='--metric'||args[2]!=='--telemetry'||!['jobs-per-run','runs-per-change'].includes(args[1])) throw Error('usage: --metric jobs-per-run|runs-per-change --telemetry PATH');
// onebudgetspec executes from the registration directory; the advertised
// repository-root command uses .github/.telemetry instead. Both name the receipt.
const path=resolve(args[3]);
const telemetry=JSON.parse(readFileSync(path,'utf8'));
if(telemetry.schema_version!==1||telemetry.sourceDigest!==digest()||!telemetry.scenarios?.length||!telemetry.runs?.length||!telemetry.lifecycles?.length) throw Error('missing or stale macOS telemetry: run github-workflows:test');
const mac=run=>run.rows.filter(row=>row.requested&&row.runner.startsWith('macos-'));
const measurements=args[1]==='jobs-per-run'?telemetry.runs.map(run=>({value:mac(run).length,detail:run})):telemetry.lifecycles.map(lifecycle=>({value:lifecycle.runs.filter(run=>mac(run).length).length,detail:lifecycle}));
const maximum=Math.max(...measurements.map(m=>m.value));
const detail=JSON.stringify({sourceDigest:telemetry.sourceDigest,workload:{eventClasses:[...new Set(telemetry.scenarios.map(s=>s.class))],diffs:[...new Set(telemetry.scenarios.map(s=>s.diff))],ownership:['same-repository','fork'],manual:'explicit opt-in excluded from lifecycle'},worst:measurements.filter(m=>m.value===maximum).map(m=>m.detail)},null,2);
report(maximum,detail);
console.log(JSON.stringify({metric:args[1],value:maximum,telemetry:path.replace(root+'/','')}));
