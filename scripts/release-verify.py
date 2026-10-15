#!/usr/bin/env python3
"""Hold a staged release directory to exactly the artifacts its targets publish.

Run by release.yml's Darwin job before anything uploads, and by tests/packaging.rs
over the same staged output. It reads every artifact rather than trusting the
packager's receipt alone: the directory holds nothing but each target's archive,
checksum, receipt, wheel and npm carrier, and every payload in them is the one
compiled binary, built for that target's own format and CPU. Executes nothing.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import sys
import tarfile
import zipfile

sys.dont_write_bytecode = True  # leave no __pycache__ beside the scripts
from release_artifacts import TARGETS, executable_format  # noqa: E402


def fail(message):
    print(f'release-verify: {message}', file=sys.stderr)
    print('ACTION: rebuild the staged directory with bash scripts/release-build.sh, staging only the declared targets', file=sys.stderr)
    sys.exit(2)


parser = argparse.ArgumentParser()
parser.add_argument('--target', required=True, action='append', choices=sorted(TARGETS))
parser.add_argument('--tag', required=True)
parser.add_argument('--dir', required=True, type=Path)
args = parser.parse_args()
if not re.fullmatch(r'v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?', args.tag):
    parser.error('--tag must be a vX.Y.Z release tag')
if not args.dir.is_absolute() or not args.dir.is_dir():
    parser.error('--dir must be an existing absolute staged artifact directory')
version = args.tag[1:]
staged = {path.name for path in args.dir.iterdir() if path.is_file()}
expected = set()


def payload(what, data, target):
    found = executable_format(data)
    if found != TARGETS[target]['executable']:
        fail(f'{what} is built for {"/".join(found)}, not {"/".join(TARGETS[target]["executable"])} as {target} ships')
    return hashlib.sha256(data).hexdigest()


for target in args.target:
    facts = TARGETS[target]
    receipt_name = f'{target}.json'
    if receipt_name not in staged:
        fail(f'missing receipt {receipt_name}')
    receipt = json.loads((args.dir / receipt_name).read_text())
    if not isinstance(receipt, dict) or set(receipt) != {'target', 'version', 'tag', 'binarySha256', 'archive', 'wheel'} or not all(isinstance(value, str) for value in receipt.values()):
        fail(f'{receipt_name} does not have the packager receipt schema')
    if (receipt['target'], receipt['version'], receipt['tag']) != (target, version, args.tag):
        fail(f'{receipt_name} records {receipt["target"]} {receipt["tag"]}, not {target} {args.tag}')
    archive = f'oneagentgraph-{args.tag}-{target}.tar.gz'
    wheel = receipt['wheel']
    if receipt['archive'] != archive or not re.fullmatch(rf'oneagentgraph_cli-[A-Za-z0-9.+!_]+-py3-none-{re.escape(facts["wheel"])}\.whl', wheel):
        fail(f'{receipt_name} names {receipt["archive"]} and {wheel}, not the {target} archive and {facts["wheel"]} wheel')
    carrier = f'oneagentgraph-cli-{facts["npm_os"]}-{facts["npm_cpu"]}-{version}.tgz'
    names = {receipt_name, archive, f'{archive}.sha256', wheel, carrier}
    missing = sorted(names - staged)
    if missing:
        fail(f'missing staged {target} artifacts: {", ".join(missing)}')
    expected |= names
    sha = receipt['binarySha256']

    data = (args.dir / archive).read_bytes()
    if (args.dir / f'{archive}.sha256').read_text() != f'{hashlib.sha256(data).hexdigest()}  {archive}\n':
        fail(f'{archive}.sha256 does not match {archive}')
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        if sorted(tar.getnames()) != ['CHANGELOG.md', 'LICENSE', 'README.md', 'oneagentgraph']:
            fail(f'{archive} carries {sorted(tar.getnames())}, not the binary and its three documents')
        if payload(f'{archive} payload', tar.extractfile('oneagentgraph').read(), target) != sha:
            fail(f'{archive} carries a different binary than {receipt_name} records')

    with zipfile.ZipFile(args.dir / wheel) as z:
        scripts = [name for name in z.namelist() if '.data/scripts/' in name]
        if len(scripts) != 1 or not scripts[0].endswith('/oneagentgraph'):
            fail(f'{wheel} must carry exactly one oneagentgraph script')
        if payload(f'{wheel} payload', z.read(scripts[0]), target) != sha:
            fail(f'{wheel} carries a different binary than {receipt_name} records')
        if not z.getinfo(scripts[0]).external_attr >> 16 & 0o111:
            fail(f'{wheel} script is not executable')
        metadata = z.read(next(name for name in z.namelist() if name.endswith('.dist-info/METADATA'))).decode()
        tags = z.read(next(name for name in z.namelist() if name.endswith('.dist-info/WHEEL'))).decode()
        if 'Name: oneagentgraph-cli\n' not in metadata or f'Tag: py3-none-{facts["wheel"]}\n' not in tags:
            fail(f'{wheel} metadata is not oneagentgraph-cli tagged py3-none-{facts["wheel"]}')

    with tarfile.open(args.dir / carrier) as npm:
        manifest = json.load(npm.extractfile('package/package.json'))
        if (manifest.get('name'), manifest.get('version'), manifest.get('os'), manifest.get('cpu')) != (f'oneagentgraph-cli-{facts["npm_os"]}-{facts["npm_cpu"]}', version, [facts['npm_os']], [facts['npm_cpu']]):
            fail(f'{carrier} manifest is not the {target} carrier at {version}')
        if payload(f'{carrier} payload', npm.extractfile('package/bin/oneagentgraph').read(), target) != sha:
            fail(f'{carrier} carries a different binary than {receipt_name} records')

extra = sorted(staged - expected)
if extra:
    fail(f'unexpected staged artifacts would publish: {", ".join(extra)}')
print(json.dumps(sorted(expected)))
