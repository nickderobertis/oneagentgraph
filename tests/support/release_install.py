"""Real native archive/wheel/npm installs with no registry or publication."""
import os
from pathlib import Path
import subprocess
import sys
import tarfile

out = Path(sys.argv[1])
version = sys.argv[2]
def run(argv, **kwargs):
    return subprocess.check_output(argv, text=True, stderr=subprocess.STDOUT, **kwargs)
def smoke(directory):
    run(['bash', 'scripts/smoke-published.sh', '--expect-version', version, '--label', 'local staged release product'], env={**os.environ, 'PATH': str(directory) + os.pathsep + os.environ['PATH']})
archive = out / 'archive'
archive.mkdir()
with tarfile.open(next(out.glob('*.tar.gz'))) as tar:
    tar.extractall(archive, filter='data')
smoke(archive)
venv = out / 'venv'
run(['python3', '-m', 'venv', str(venv)])
run([str(venv / 'bin/python'), '-m', 'pip', 'install', '--no-index', str(next(out.glob('*.whl')))])
smoke(venv / 'bin')
project = out / 'installed'
run(['npm', 'install', '--prefix', str(project), '--offline', '--no-audit', '--no-fund', '--omit=optional', *map(str,out.glob('*.tgz'))])
smoke(project / 'node_modules/oneagentgraph-cli-linux-x64/bin')
