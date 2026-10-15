#!/usr/bin/env python3
"""Wrap one compiled binary in an archive and CLI wheel, without rebuilding."""
import argparse
import base64
import csv
import hashlib
import io
import json
from email.parser import Parser
import tempfile
from pathlib import Path
import re
import sys
import tarfile
import tomllib
import zipfile

sys.dont_write_bytecode = True  # leave no __pycache__ beside the scripts
from release_artifacts import TARGETS, MaturinUnavailable, executable_format, maturin_dist_info  # noqa: E402

parser = argparse.ArgumentParser()
parser.add_argument('--target', required=True, choices=sorted(TARGETS))
parser.add_argument('--binary', required=True, type=Path)
parser.add_argument('--out', required=True, type=Path)
parser.add_argument('--tag', required=True)
args = parser.parse_args()
if not args.out.is_absolute() or (args.out.exists() and not args.out.is_dir()):
    parser.error('--out must be an absolute artifact directory; pass a new directory or an existing directory')
root = Path(__file__).resolve().parent.parent
project = tomllib.loads((root / 'pyproject.toml').read_text()).get('project')
package = tomllib.loads((root / 'Cargo.toml').read_text()).get('package')
if not isinstance(project, dict) or not all(isinstance(project.get(key), str) and project[key] for key in ['name', 'requires-python', 'description']):
    parser.error('pyproject.toml must define project name, requires-python and description strings; restore the project metadata')
if not re.fullmatch(r'[A-Za-z0-9]+(?:[-_.][A-Za-z0-9]+)*', project['name']):
    parser.error('project name is not a valid distribution name; restore pyproject.toml project.name')
if not isinstance(package, dict) or not isinstance(package.get('version'), str):
    parser.error('Cargo.toml must define a package version string; restore package.version')
version = package['version']
if args.tag != f'v{version}' or not re.fullmatch(r'\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?', version):
    parser.error('release tag must equal v plus the Cargo.toml version')
if not args.binary.is_file():
    parser.error('--binary must name an existing regular file; compile the requested target before packaging')
binary = args.binary.read_bytes()
found = executable_format(binary)
if found != TARGETS[args.target]['executable']:
    parser.error(f'--binary is built for {"/".join(found)}, not {"/".join(TARGETS[args.target]["executable"])} as {args.target} ships; build it with --target {args.target}')
args.out.mkdir(parents=True, exist_ok=True)
archive = args.out / f'oneagentgraph-{args.tag}-{args.target}.tar.gz'
with tarfile.open(archive, 'w:gz') as tar:
    tar.add(args.binary, arcname='oneagentgraph')
    for name in ['README.md', 'LICENSE', 'CHANGELOG.md']:
        tar.add(root / name, arcname=name)
(args.out / f'{archive.name}.sha256').write_text(f'{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n')
# Maturin's existing PEP 517 metadata producer exposes the packaging seam
# without a build. Keep its metadata and version normalization authoritative.
with tempfile.TemporaryDirectory(dir=args.out) as metadata_dir:
    try:
        dist_info = maturin_dist_info(root, args.target, Path(metadata_dir))
    except MaturinUnavailable as error:
        parser.error(str(error))
    info = dist_info.name
    metadata = Parser().parsestr((dist_info / 'METADATA').read_text())
    wheel_version = metadata.get('Version')
    if not wheel_version or not re.fullmatch(r'[A-Za-z0-9.+!_-]+', wheel_version):
        parser.error('maturin produced no safe wheel version; inspect project metadata')
    name = info.removesuffix('.dist-info').removesuffix('-' + wheel_version)
    tags = [line.removeprefix('Tag: ') for line in (dist_info / 'WHEEL').read_text().splitlines() if line.startswith('Tag: ')]
    if len(tags) != 1 or not re.fullmatch(r'py3-none-[A-Za-z0-9_.]+', tags[0]):
        parser.error('maturin produced an unsupported binary-wheel tag; inspect target metadata')
    tag = tags[0]
    files = {f'{info}/{path.relative_to(dist_info).as_posix()}': path.read_bytes() for path in dist_info.rglob('*') if path.is_file()}
files[f'{name}-{wheel_version}.data/scripts/oneagentgraph'] = binary
rows = [[path, 'sha256=' + base64.urlsafe_b64encode(hashlib.sha256(data).digest()).decode().rstrip('='), str(len(data))] for path, data in files.items()]
rows.append([f'{info}/RECORD', '', ''])
record = io.StringIO()
csv.writer(record, lineterminator='\n').writerows(rows)
files[f'{info}/RECORD'] = record.getvalue().encode()
wheel = args.out / f'{name}-{wheel_version}-{tag}.whl'
with zipfile.ZipFile(wheel, 'w', zipfile.ZIP_DEFLATED) as z:
    for path, data in files.items():
        entry = zipfile.ZipInfo(path)
        entry.external_attr = (0o100755 if '/scripts/' in path else 0o100644) << 16
        z.writestr(entry, data)
(args.out / f'{args.target}.json').write_text(json.dumps({'target': args.target, 'version': version, 'tag': args.tag, 'binarySha256': hashlib.sha256(binary).hexdigest(), 'archive': archive.name, 'wheel': wheel.name}) + '\n')
