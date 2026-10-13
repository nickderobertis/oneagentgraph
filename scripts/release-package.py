#!/usr/bin/env python3
"""Wrap one compiled Darwin binary in an archive and CLI wheel, without rebuilding."""
import argparse
import base64
import csv
import hashlib
import io
import json
from pathlib import Path
import re
import tarfile
import tomllib
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument('--target', required=True, choices=['x86_64-apple-darwin', 'aarch64-apple-darwin', 'x86_64-unknown-linux-gnu'])
parser.add_argument('--binary', required=True, type=Path)
parser.add_argument('--out', required=True, type=Path)
parser.add_argument('--tag', required=True)
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
project = tomllib.loads((root / 'pyproject.toml').read_text())['project']
version = tomllib.loads((root / 'Cargo.toml').read_text())['package']['version']
if args.tag != f'v{version}' or not re.fullmatch(r'\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?', version):
    parser.error('release tag must equal v plus the Cargo.toml version')
binary = args.binary.read_bytes()
args.out.mkdir(parents=True, exist_ok=True)
archive = args.out / f'oneagentgraph-{args.tag}-{args.target}.tar.gz'
with tarfile.open(archive, 'w:gz') as tar:
    tar.add(args.binary, arcname='oneagentgraph')
    for name in ['README.md', 'LICENSE', 'CHANGELOG.md']:
        tar.add(root / name, arcname=name)
(args.out / f'{archive.name}.sha256').write_text(f'{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n')
arch = 'x86_64' if args.target.startswith('x86_64') else 'arm64'
# Match Rust's minimum supported deployment versions for these targets.
platform = f'macosx_{"10_12" if arch == "x86_64" else "11_0"}_{arch}'
if args.target == 'x86_64-unknown-linux-gnu':
    platform = 'linux_x86_64'
name = project['name'].replace('-', '_')
# PEP 440 spells SemVer prereleases differently; this repository publishes plain
# release versions. Refuse a suffix rather than emit invalid wheel metadata.
if not re.fullmatch(r'\d+\.\d+\.\d+', version):
    parser.error('CLI wheel packaging requires a plain release version')
info = f'{name}-{version}.dist-info'
files = {
    f'{name}-{version}.data/scripts/oneagentgraph': binary,
    f'{info}/METADATA': (f'Metadata-Version: 2.1\nName: {project["name"]}\nVersion: {version}\nRequires-Python: {project["requires-python"]}\nSummary: {project["description"]}\nLicense: MIT\n\n' + (root / 'README.md').read_text()).encode(),
    f'{info}/WHEEL': f'Wheel-Version: 1.0\nGenerator: oneagentgraph release-package\nRoot-Is-Purelib: false\nTag: py3-none-{platform}\n'.encode(),
    f'{info}/licenses/LICENSE': (root / 'LICENSE').read_bytes(),
}
rows = [[path, 'sha256=' + base64.urlsafe_b64encode(hashlib.sha256(data).digest()).decode().rstrip('='), str(len(data))] for path, data in files.items()]
rows.append([f'{info}/RECORD', '', ''])
record = io.StringIO()
csv.writer(record, lineterminator='\n').writerows(rows)
files[f'{info}/RECORD'] = record.getvalue().encode()
wheel = args.out / f'{name}-{version}-py3-none-{platform}.whl'
with zipfile.ZipFile(wheel, 'w', zipfile.ZIP_DEFLATED) as z:
    for path, data in files.items():
        entry = zipfile.ZipInfo(path)
        entry.external_attr = (0o100755 if '/scripts/' in path else 0o100644) << 16
        z.writestr(entry, data)
(args.out / f'{args.target}.json').write_text(json.dumps({'target': args.target, 'version': version, 'tag': args.tag, 'binarySha256': hashlib.sha256(binary).hexdigest(), 'archive': archive.name, 'wheel': wheel.name}) + '\n')
