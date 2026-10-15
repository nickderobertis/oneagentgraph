import { readFileSync, appendFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { isDeepStrictEqual } from 'node:util';
import { parse as toml } from '../node_modules/smol-toml/dist/index.js';

// This prefix is the repository's release-plz convention. Authors are not an
// authority: its PAT and an ordinary developer share the same login.
const prefix = 'release-plz-';
const version = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;
function removeVersion(value) {
  if (!version.test(value.version)) throw Error('missing or invalid package version');
  delete value.version;
}
const git = (...args) => execFileSync('git', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
function withoutVersion(path, source) {
  if (path === 'Cargo.toml') {
    const value = toml(source);
    if (!value.package?.version) throw Error('missing package version');
    removeVersion(value.package);
    return value;
  }
  if (path === 'Cargo.lock') {
    const value = toml(source);
    // Dependency versions/checksums remain significant. Only our own root
    // package version may move in a release-only change.
    const own = value.package.filter(p => p.name === 'oneagentgraph' && !p.source);
    if (own.length !== 1) throw Error('missing or ambiguous root package');
    removeVersion(own[0]);
    return value;
  }
  if (['package.json', 'package-lock.json', 'npm/oneagentgraph-cli/package.json'].includes(path)) {
    const value = JSON.parse(source);
    removeVersion(value);
    if (path === 'package-lock.json' && value.packages?.['']) removeVersion(value.packages['']);
    // Optional dependency pins in the generated launcher are release versions,
    // but their names must remain unchanged.
    if (path === 'npm/oneagentgraph-cli/package.json') {
      for (const name of Object.keys(value.optionalDependencies ?? {})) {
        if (!/^oneagentgraph-cli-(linux-(x64|arm64)|darwin-arm64|win32-x64)$/.test(name) || !version.test(value.optionalDependencies[name])) throw Error('non-version launcher dependency');
        value.optionalDependencies[name] = '<version>';
      }
    }
    return value;
  }
  if (path === 'pyproject.toml') {
    const value = toml(source);
    if (value.project?.version !== undefined) removeVersion(value.project);
    return value;
  }
  throw Error(`non-release file: ${path}`);
}
export function classify(event, base, head, eventName) {
  if (eventName && eventName !== 'pull_request') {
    const known = ['push', 'release', 'workflow_run', 'schedule', 'repository_dispatch', 'workflow_dispatch'];
    return { isReleaseOnly: false, macosRequired: !known.includes(eventName), reason: `${known.includes(eventName) ? 'non-PR' : 'unrecognised CI'} event: ${eventName}` };
  }
  const required = reason => ({ isReleaseOnly: false, macosRequired: true, reason });
  try {
    if (!event?.pull_request || (eventName && eventName !== 'pull_request')) return required('unknown PR payload');
    const pr = event.pull_request;
    if (!event.repository?.full_name || pr.head?.repo?.full_name !== event.repository.full_name || pr.base?.repo?.full_name !== event.repository.full_name || !pr.head?.ref?.startsWith(prefix)) return required('ordinary or fork PR');
    const mergeBase = git('merge-base', base, head).trim();
    const entries = git('diff', '--name-status', '-z', mergeBase, head).split('\0');
    let changed = 0;
    for (let i = 0; i < entries.length - 1; i += 2) {
      const [status, path] = [entries[i], entries[i + 1]];
      if (status !== 'M') return required(`non-version file operation: ${status} ${path}`);
      changed++;
      if (path === 'CHANGELOG.md') continue;
      const before = withoutVersion(path, git('show', `${mergeBase}:${path}`));
      const after = withoutVersion(path, git('show', `${head}:${path}`));
      if (!isDeepStrictEqual(before, after)) return required(`non-version content: ${path}`);
    }
    if (!changed) return required('empty diff is not a release');
    return { isReleaseOnly: true, macosRequired: false, reason: 'same-repository release-plz branch with semantic version/changelog-only diff' };
  } catch (error) { return required(`unreadable PR classification: ${error.message}`); }
}

if (process.argv[1]?.endsWith('/macos-policy.mjs')) {
  const args = process.argv.slice(2);
  if (args[0] !== 'classify') throw Error('usage: classify --event JSON --base REF --head REF');
  const options = {};
  for (let i = 1; i < args.length; i += 2) {
    if (!['--event', '--base', '--head'].includes(args[i]) || !args[i + 1] || options[args[i]]) throw Error('invalid classify arguments');
    options[args[i]] = args[i + 1];
  }
  let event;
  try { event = JSON.parse(readFileSync(options['--event'], 'utf8')); } catch { event = null; }
  const result = classify(event, options['--base'], options['--head'], process.env.GITHUB_EVENT_NAME ?? event?.event_name);
  console.log(JSON.stringify(result));
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `macosRequired=${result.macosRequired}\nisReleaseOnly=${result.isReleaseOnly}\n`);
}
