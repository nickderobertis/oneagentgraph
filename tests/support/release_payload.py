"""Inspect package contents without executing a non-native payload."""
import hashlib
import json
from pathlib import Path
import sys
import tarfile
import zipfile

binary, target, version, directory = sys.argv[1:]
out = Path(directory)
expected = hashlib.sha256(Path(binary).read_bytes()).hexdigest()
receipt = json.loads((out / f'{target}.json').read_text())
assert receipt['target'] == target and receipt['version'] == version
assert receipt['tag'] == f'v{version}' and receipt['binarySha256'] == expected
archive = out / receipt['archive']
assert hashlib.sha256(archive.read_bytes()).hexdigest() in (out / f'{archive.name}.sha256').read_text()
with tarfile.open(archive) as tar:
    assert set(tar.getnames()) == {'oneagentgraph', 'README.md', 'LICENSE', 'CHANGELOG.md'}
    assert hashlib.sha256(tar.extractfile('oneagentgraph').read()).hexdigest() == expected
with zipfile.ZipFile(out / receipt['wheel']) as wheel:
    path = next(p for p in wheel.namelist() if '.data/scripts/' in p)
    assert hashlib.sha256(wheel.read(path)).hexdigest() == expected
    assert wheel.getinfo(path).external_attr >> 16 & 0o111
    metadata = wheel.read(next(p for p in wheel.namelist() if p.endswith('/METADATA'))).decode()
    assert f'Version: {version}\n' in metadata and 'Name: oneagentgraph-cli\n' in metadata
    tag = {'x86_64-apple-darwin':'macosx_10_12_x86_64', 'aarch64-apple-darwin':'macosx_11_0_arm64', 'x86_64-unknown-linux-gnu':'linux_x86_64'}[target]
    assert f'Tag: py3-none-{tag}\n' in wheel.read(next(p for p in wheel.namelist() if p.endswith('/WHEEL'))).decode()
with tarfile.open(next(out.glob('*.tgz'))) as npm:
    assert hashlib.sha256(npm.extractfile('package/bin/oneagentgraph').read()).hexdigest() == expected
    manifest = json.load(npm.extractfile('package/package.json'))
    assert manifest['version'] == version
    assert manifest['os'] == ['darwin' if 'darwin' in target else 'linux']
    assert manifest['cpu'] == ['arm64' if target.startswith('aarch64') else 'x64']
