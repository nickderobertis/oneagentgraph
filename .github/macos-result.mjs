// GitHub needs/results is the only doubled boundary in the subprocess tests.
const needs = JSON.parse(process.argv[2]);
const { macosRequired: required, rust: affected } = needs.changes?.outputs ?? {};
const { changes, gate, macos } = Object.fromEntries(['changes', 'gate', 'macos'].map(key => [key, needs[key]?.result]));
if (changes !== 'success' || gate !== 'success') throw Error('prerequisite failed, cancelled, or missing');
if (!['true', 'false'].includes(required) || !['true', 'false'].includes(affected)) throw Error('missing policy outputs');
if (required === 'true' && affected === 'true') {
  if (macos !== 'success') throw Error('required macOS proof failed, cancelled, or missing');
  console.log('macOS cross and install assertions passed');
} else {
  if (macos !== 'skipped') throw Error('omitted lane must be explicitly skipped');
  console.log(required === 'false' ? 'policy authorizes omission; macOS assertions did not run' : 'no affected Rust project; macOS assertions did not run');
}
