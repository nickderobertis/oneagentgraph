import { check } from '@onebudgetspec/sdk';
const result=await check({paths:['.github/budgets.yaml']});
console.log(JSON.stringify(result.results.map(({id,actual,verdict,error})=>({id,actual,verdict,error})),null,2));
if(result.results.some(result=>result.verdict!=='within')) process.exitCode=1;
