// Repository-private resolver. Expressions use GitHub's published evaluator;
// unsupported workflows, matrices and runners fail closed with their source site.
import { readFileSync, readdirSync, mkdtempSync, rmSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { tmpdir } from 'node:os';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { parse } from 'yaml';
import { parse as toml } from '../node_modules/smol-toml/dist/index.js';
import { Lexer, Parser, Evaluator, data } from '@actions/expressions';
export const root = resolve(import.meta.dirname, '..');
const read = path => readFileSync(join(root, path), 'utf8');
const convert = value => {
  if (value === null || value === undefined) return new data.Null();
  if (typeof value === 'string') return new data.StringData(value);
  if (typeof value === 'boolean') return new data.BooleanData(value);
  if (typeof value === 'number') return new data.NumberData(value);
  if (Array.isArray(value)) return new data.Array(...value.map(convert));
  return new data.Dictionary(...Object.entries(value).map(([key, value]) => ({key, value: convert(value)})));
};
const plain = value => JSON.parse(JSON.stringify(value, data.replacer));
export function evaluate(source, context, outcomes = []) {
  const expression = source.trim().replace(/^\$\{\{\s*/, '').replace(/\s*\}\}$/, '');
  const funcs = new Map(['always', 'success', 'failure', 'cancelled'].map(name => [name, {
    name, minArgs: 0, maxArgs: 0,
    call: () => convert(name === 'always' || (name === 'success' && outcomes.every(x => x === 'success')) || (name === 'failure' && outcomes.includes('failure')) || (name === 'cancelled' && outcomes.includes('cancelled'))),
  }]));
  const ast = new Parser(new Lexer(expression).lex().tokens, Object.keys(context), [...funcs.values()]).parse();
  return plain(new Evaluator(ast, convert(context), funcs).evaluate());
}
export function render(value, context, outcomes) {
  if (typeof value !== 'string') return value;
  if (/^\$\{\{[\s\S]*\}\}$/.test(value) && (value.match(/\$\{\{/g) ?? []).length === 1) return evaluate(value, context, outcomes);
  return value.replace(/\$\{\{[\s\S]*?\}\}/g, expression => String(evaluate(expression, context, outcomes)));
}
export function matrices(matrix, context) {
  if (!matrix) return [{}];
  matrix = render(matrix, context);
  if (!matrix || typeof matrix !== 'object' || Array.isArray(matrix)) throw Error('unresolved matrix');
  let rows = [{}];
  const axes = Object.entries(matrix).filter(([key]) => !['include', 'exclude'].includes(key));
  for (const [key, raw] of axes) {
    const values = render(raw, context);
    if (!Array.isArray(values) || !values.length) throw Error(`unresolved matrix axis ${key}`);
    rows = rows.flatMap(row => values.map(value => ({...row, [key]: value})));
  }
  const excluded = render(matrix.exclude ?? [], context);
  rows = rows.filter(row => !excluded.some(exclude => Object.entries(exclude).every(([k,v]) => row[k] === v)));
  const original = structuredClone(rows);
  const includes = render(matrix.include ?? [], context);
  if (!Array.isArray(includes)) throw Error('unresolved matrix include');
  if (!axes.length) return includes.length ? includes : rows;
  for (const include of includes) {
    let added = false;
    original.forEach((row, i) => {
      if (Object.keys(include).every(key => !(key in row) || row[key] === include[key])) { Object.assign(rows[i], include); added = true; }
    });
    if (!added) rows.push(include);
  }
  return rows;
}
export function workflows() {
  return readdirSync(join(root, '.github/workflows')).filter(name => /\.ya?ml$/.test(name)).sort().map(name => ({path: `.github/workflows/${name}`, doc: parse(read(`.github/workflows/${name}`))}));
}
export function digest() {
  // Include the complete tracked tree and any new untracked source,
  // excluding only generated telemetry, dependencies and this receipt itself.
  const paths = new Set(execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard'], {cwd:root,encoding:'utf8'}).trim().split('\n'));
  const hash = createHash('sha256');
  for (const path of [...paths].sort()) if (path && !path.startsWith('.github/.telemetry/')) hash.update(path).update(read(path));
  return hash.digest('hex');
}
function matches(pattern, value) {
  // The actual repository uses exact names and simple branch/tag wildcards.
  if (/[?!\[\]{}]/.test(pattern)) throw Error(`unsupported event filter: ${pattern}`);
  return new RegExp(`^${pattern.split('*').map(x => x.replace(/[.+^$()|\\]/g, '\\$&')).join('.*')}$`).test(value);
}
export function triggered(doc, scenario) {
  const on = typeof doc.on === 'string' ? {[doc.on]:null} : Array.isArray(doc.on) ? Object.fromEntries(doc.on.map(x=>[x,null])) : doc.on;
  if (!on || !(scenario.eventName in on)) return false;
  if (scenario.eventName === 'schedule') {
    if (!Array.isArray(on.schedule) || !on.schedule.every(entry => typeof entry.cron === 'string')) throw Error('unresolved schedule');
    return true;
  }
  const filter = on[scenario.eventName] ?? {};
  for (const key of Object.keys(filter)) if (!['branches','branches-ignore','tags','tags-ignore','types','workflows','inputs'].includes(key)) throw Error(`unsupported trigger filter ${key}`);
  if (filter.types && !filter.types.includes(scenario.action)) return false;
  if (filter.workflows && !filter.workflows.includes(scenario.upstream)) return false;
  for (const [type,prefix] of [['branches','refs/heads/'],['tags','refs/tags/']]) {
    const ref = scenario.eventName === 'pull_request' ? 'refs/heads/main' : scenario.ref;
    if (filter[type] && (!ref.startsWith(prefix) || !filter[type].some(p=>matches(p,ref.slice(prefix.length))))) return false;
    if (filter[`${type}-ignore`]?.some(p=>matches(p,ref.slice(prefix.length)))) return false;
  }
  return true;
}
const pin = 'nickderobertis/screencomp/.github/workflows/visual-docs-reusable.yml@v0.4.8';
export function reusable(call) {
  if (call.startsWith('./.github/workflows/')) return {path:call.slice(2),doc:parse(read(call.slice(2)))};
  const receipt = JSON.parse(read('.github/reusable/lock.json'));
  if (call !== pin || receipt.call !== call) throw Error(`unresolved reusable workflow: ${call}`);
  const source = read('.github/reusable/visual-docs.yml');
  if (createHash('sha256').update(source).digest('hex') !== receipt.sha256) throw Error('reusable snapshot digest mismatch');
  return {path:call,doc:parse(source)};
}
const matrixCache = new Map();
function screencompMatrix(doc, inputs) {
  const key = JSON.stringify([doc.jobs.arches, inputs, read("screencomp.toml")]);
  if (matrixCache.has(key)) return matrixCache.get(key);
  // Execute the pinned workflow's own output-producing command. Its input
  // override is the committed capture configuration, avoiding a CLI install.
  const step = doc.jobs.arches.steps.find(step => step.id === 'read');
  if (!step?.run) throw Error('unresolved screencomp matrix producer');
  const dir = mkdtempSync(join(process.env.ONEPIPELINE_NODE_SCRATCH_DIR ?? tmpdir(), 'arches-'));
  try {
    const output = join(dir,'output');
    execFileSync('bash', ['-c',step.run], {cwd:root,env:{...process.env,GITHUB_OUTPUT:output,ARCHES_OVERRIDE:JSON.stringify(toml(read('screencomp.toml')).capture.arches),PROJECTS_INPUT:inputs.projects??'',DEFAULT_TITLE:inputs['gallery-title']},stdio:['ignore','pipe','pipe']});
    const line = readFileSync(output,'utf8').trim();
    if (!line.startsWith('matrix=')) throw Error('matrix producer emitted no output');
    JSON.parse(line.slice(7));
    matrixCache.set(key, line.slice(7));
    return line.slice(7);
  } finally { rmSync(dir,{recursive:true,force:true}); }
}
export function resolveRun(workflow, scenario, policy, {overrides={},prefix='',inputs={},stack=[]}={}) {
  if (stack.includes(workflow.path)) throw Error(`reusable workflow cycle: ${workflow.path}`);
  const jobs = workflow.doc.jobs;
  if (!jobs) throw Error(`no jobs: ${workflow.path}`);
  const github = {event_name:scenario.eventName,event:{action:scenario.action,workflow_run:{conclusion:'success'},...scenario.event},repository:'nickderobertis/oneagentgraph',head_ref:scenario.event?.pull_request?.head?.ref??'',base_ref:scenario.eventName==='pull_request'?'main':'',ref:scenario.ref,sha:scenario.head??'head',ref_name:scenario.ref.split('/').at(-1)};
  const outputs = {changes:{rust:String(scenario.affected??true),macosRequired:String(policy.macosRequired),isReleaseOnly:String(policy.isReleaseOnly)}};
  if (workflow.path === pin) outputs.arches = {matrix:screencompMatrix(workflow.doc,inputs)};
  const done = new Map();
  const rows = [];
  function job(id) {
    if (done.has(id)) return done.get(id);
    if (!jobs[id]) throw Error(`missing prerequisite ${id} in ${workflow.path}`);
    done.set(id,{result:'resolving'});
    const spec=jobs[id];
    const needIds = spec.needs ? [].concat(spec.needs) : [];
    const needs = Object.fromEntries(needIds.map(key=>[key,job(key)]));
    if (Object.values(needs).some(n=>n.result==='resolving')) throw Error('needs cycle');
    const outcomes = Object.values(needs).map(n=>n.result);
    const context={github,needs,inputs,vars:{PYPI_PUBLISH:scenario.pypi?'true':'false',NPM_PUBLISH:scenario.npm?'true':'false'},matrix:{}};
    const condition=spec.if??'success()';
    const statusExplicit=/\b(always|success|failure|cancelled)\s*\(/.test(condition);
    const active=(statusExplicit || outcomes.every(x=>x==='success')) && Boolean(evaluate(condition,context,outcomes));
    // GitHub evaluates job if before expanding a dynamic matrix. The pinned
    // producer is still executed for the static contract of skipped lanes;
    // these rows explicitly remain unrequested.
    const matrixContext = !active && outputs.arches && needs.arches ? {...context, needs:{...needs, arches:{...needs.arches, outputs:outputs.arches}}} : context;
    const expanded=matrices(spec.strategy?.matrix,matrixContext);
    for (const matrix of expanded) {
      const ctx={...context,matrix};
      let name=spec.name ? render(spec.name,ctx,outcomes) : id;
      if (!spec.name && spec.strategy?.matrix) name += ` (${Object.values(matrix).filter(value => value !== '').join(', ')})`;
      if (spec.uses) {
        const called=reusable(spec.uses);
        const defaults=Object.fromEntries(Object.entries(called.doc.on.workflow_call.inputs??{}).map(([k,v])=>[k,v.default??null]));
        const passed=Object.fromEntries(Object.entries(spec.with??{}).map(([k,v])=>[k,render(v,ctx,outcomes)]));
        const nested=resolveRun(called,scenario,policy,{prefix:`${prefix}${name} / `,inputs:{...defaults,...passed},stack:[...stack,workflow.path]});
        rows.push(...nested.map(row=>({...row,requested:active&&row.requested,workflow:workflow.path,call:spec.uses})));
      } else {
        const runner=render(spec['runs-on'],ctx,outcomes);
        if (typeof runner !== 'string' || !/^(ubuntu|macos|windows)-[a-z0-9.-]+$/.test(runner)) throw Error(`unresolved runner ${workflow.path}:${id}: ${JSON.stringify(runner)}`);
        rows.push({workflow:workflow.path,event:scenario.id,job:id,name:`${prefix}${name}`,runner,target:matrix.target??matrix.arch??'',requested:active,matrix,needs:needIds});
      }
    }
    const result=overrides[id]??(active?'success':'skipped');
    const out={result,outputs:result==='success'?(outputs[id]??{}):{}};
    done.set(id,out);return out;
  }
  for (const id of Object.keys(jobs)) job(id);
  return rows;
}
