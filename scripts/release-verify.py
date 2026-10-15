#!/usr/bin/env python3
"""Hold a staged release directory to exactly the artifacts its targets publish.

Run by release.yml's Darwin job (through scripts/release-rehearse.sh) before
anything uploads, and by tests/packaging.rs over the Linux staged output and an
arm64 Darwin set packaged on Linux. It reads every artifact rather than trusting the
packager's receipt alone: the directory holds nothing but each target's archive,
checksum, receipt, wheel and npm carrier, and every payload in them is the one
compiled binary, built for that target's own format and CPU. It never executes
a staged payload; the one process it runs is maturin, to learn the wheel version.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import sys
import tarfile
import tempfile
import tomllib
import zipfile

sys.dont_write_bytecode = True  # leave no __pycache__ beside the scripts
from release_artifacts import TARGETS, MaturinUnavailable, executable_format, maturin_dist_info  # noqa: E402


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
root = Path(__file__).resolve().parent.parent
if tomllib.loads((root / 'Cargo.toml').read_text()).get('package', {}).get('version') != version:
    fail(f'{args.tag} is not the version this checkout builds; verify from the checkout of the released commit')
staged = {path.name for path in args.dir.iterdir() if path.is_file()}
expected = set()


class Malformed(Exception):
    """A staged artifact that cannot be read as the format its name claims."""


def payload(what, data, target):
    found = executable_format(data)
    if found != TARGETS[target]['executable']:
        fail(f'{what} is built for {"/".join(found)}, not {"/".join(TARGETS[target]["executable"])} as {target} ships')
    return hashlib.sha256(data).hexdigest()


def member(container, name, label):
    """The bytes of member `name`, or Malformed when it is absent or, in a tar,
    a directory or a link `extractfile` would otherwise follow."""
    try:
        info = container.getmember(name) if isinstance(container, tarfile.TarFile) else container.getinfo(name)
    except KeyError:
        raise Malformed(f'{label} has no {name}') from None
    if not isinstance(info, tarfile.TarInfo):
        return container.read(info)  # a zip's only non-file is a directory, whose name ends in '/'
    if not info.isfile():
        raise Malformed(f'{label} member {name} is not a regular file')
    return container.extractfile(info).read()


def executable(tar, name, label):
    """The bytes of tar member `name`, which must be a regular file a user can run."""
    data = member(tar, name, label)
    if not tar.getmember(name).mode & 0o111:
        fail(f'{label} member {name} is not executable')
    return data


def verify(target):
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
    # The wheel's version is maturin's PEP 440 spelling of the release version;
    # ask maturin for it rather than trust the receipt or re-derive the spelling.
    with tempfile.TemporaryDirectory() as metadata_dir:
        try:
            dist_info = maturin_dist_info(root, target, Path(metadata_dir))
        except MaturinUnavailable as error:
            fail(str(error))
        declared = re.findall(r'^Version: (.*)$', (dist_info / 'METADATA').read_text(), re.M)
    if len(declared) != 1:
        fail(f'maturin declares {len(declared)} wheel versions for {target}; inspect project metadata')
    wheel_version = declared[0]
    wheel = f'oneagentgraph_cli-{wheel_version}-py3-none-{facts["wheel"]}.whl'
    if (receipt['archive'], receipt['wheel']) != (archive, wheel):
        fail(f'{receipt_name} names {receipt["archive"]} and {receipt["wheel"]}, not {archive} and {wheel}')
    carrier = f'oneagentgraph-cli-{facts["npm_os"]}-{facts["npm_cpu"]}-{version}.tgz'
    names = {receipt_name, archive, f'{archive}.sha256', wheel, carrier}
    missing = sorted(names - staged)
    if missing:
        fail(f'missing staged {target} artifacts: {", ".join(missing)}')
    expected.update(names)
    sha = receipt['binarySha256']

    data = (args.dir / archive).read_bytes()
    if (args.dir / f'{archive}.sha256').read_text() != f'{hashlib.sha256(data).hexdigest()}  {archive}\n':
        fail(f'{archive}.sha256 does not match {archive}')
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        if sorted(tar.getnames()) != ['CHANGELOG.md', 'LICENSE', 'README.md', 'oneagentgraph']:
            fail(f'{archive} carries {sorted(tar.getnames())}, not the binary and its three documents')
        if payload(f'{archive} payload', executable(tar, 'oneagentgraph', archive), target) != sha:
            fail(f'{archive} carries a different binary than {receipt_name} records')

    with zipfile.ZipFile(args.dir / wheel) as z:
        # pip installs a command only from the top-level data directory.
        script = f'oneagentgraph_cli-{wheel_version}.data/scripts/oneagentgraph'
        scripts = [name for name in z.namelist() if '.data/scripts/' in name]
        if scripts != [script]:
            fail(f'{wheel} must carry exactly one script, {script}, not {", ".join(scripts) or "none"}')
        if payload(f'{wheel} payload', member(z, script, wheel), target) != sha:
            fail(f'{wheel} carries a different binary than {receipt_name} records')
        if not z.getinfo(script).external_attr >> 16 & 0o111:
            fail(f'{wheel} script is not executable')
        dist_info = f'oneagentgraph_cli-{wheel_version}.dist-info'
        metadata = member(z, f'{dist_info}/METADATA', wheel).decode()
        tags = member(z, f'{dist_info}/WHEEL', wheel).decode()
        if 'Name: oneagentgraph-cli\n' not in metadata or f'Tag: py3-none-{facts["wheel"]}\n' not in tags:
            fail(f'{wheel} metadata is not oneagentgraph-cli tagged py3-none-{facts["wheel"]}')
        declared = re.findall(r'^Version: (.*)$', metadata, re.M)
        if declared != [wheel_version]:
            fail(f'{wheel} METADATA declares Version {", ".join(declared) or "none"}, not the release version {wheel_version}')

    with tarfile.open(args.dir / carrier) as npm:
        manifest = json.loads(member(npm, 'package/package.json', carrier))
        identity = tuple(manifest.get(key) for key in ['name', 'version', 'os', 'cpu']) if isinstance(manifest, dict) else None
        if identity != (f'oneagentgraph-cli-{facts["npm_os"]}-{facts["npm_cpu"]}', version, [facts['npm_os']], [facts['npm_cpu']]):
            fail(f'{carrier} manifest is not the {target} carrier at {version}')
        if payload(f'{carrier} payload', executable(npm, 'package/bin/oneagentgraph', carrier), target) != sha:
            fail(f'{carrier} carries a different binary than {receipt_name} records')


for target in args.target:
    # Each artifact is external input here; one that is truncated, corrupt or
    # not the format its name claims is a refusal like any other, not a traceback.
    try:
        verify(target)
    except (Malformed, OSError, ValueError, tarfile.TarError, zipfile.BadZipFile, EOFError) as error:
        fail(f'a staged {target} artifact is malformed: {error}')

extra = sorted(staged - expected)
if extra:
    fail(f'unexpected staged artifacts would publish: {", ".join(extra)}')
print(json.dumps(sorted(expected)))
