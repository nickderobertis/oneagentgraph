"""What each release target ships, read from an executable's own header.

Shared by scripts/release-package.py, which refuses to wrap a binary built for
another target, and scripts/release-verify.py, which checks every staged payload
before publication. Reading the header is what lets a Linux host hold a Darwin
artifact to its contract without executing it. Both also take a wheel's metadata,
and so its PEP 440 version, from maturin rather than deriving it themselves.
"""
import os
from pathlib import Path
import shlex
import shutil
import struct
import subprocess
import sys

# Rust target -> the (format, cpu) its executable must be, and its wheel / npm facts.
TARGETS = {
    'aarch64-apple-darwin': {'executable': ('mach-o', 'arm64'), 'wheel': 'macosx_11_0_arm64', 'npm_os': 'darwin', 'npm_cpu': 'arm64'},
    'x86_64-unknown-linux-gnu': {'executable': ('elf', 'x86_64'), 'wheel': 'linux_x86_64', 'npm_os': 'linux', 'npm_cpu': 'x64'},
}

_MACH_O_CPUS = {0x0100000C: 'arm64', 0x01000007: 'x86_64'}
_ELF_MACHINES = {62: 'x86_64', 183: 'aarch64'}


def executable_format(data: bytes) -> tuple[str, str]:
    """The (format, cpu) an executable's header declares, or ('unknown', '?')."""
    if data[:4] == b'\xcf\xfa\xed\xfe' and len(data) >= 8:
        (cpu,) = struct.unpack_from('<I', data, 4)
        return 'mach-o', _MACH_O_CPUS.get(cpu, f'cpu {cpu:#x}')
    if data[:4] in (b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca'):
        return 'mach-o universal', 'multiple'
    if data[:4] == b'\x7fELF' and len(data) >= 20 and data[4] == 2 and data[5] == 1:
        (machine,) = struct.unpack_from('<H', data, 18)
        return 'elf', _ELF_MACHINES.get(machine, f'machine {machine}')
    if data[:2] == b'MZ':
        return 'pe', '?'
    return 'unknown', '?'


class MaturinUnavailable(Exception):
    """maturin could not describe the wheel; the message says what to do."""


def maturin_dist_info(root: Path, target: str, into: Path) -> Path:
    """The dist-info maturin 1.14.1 writes for `target`, without a build.

    Maturin's PEP 517 metadata producer is the one source of the wheel's name,
    tags and PEP 440 version, so neither the packager nor the verifier keeps a
    copy of those rules.
    """
    candidates = [shutil.which('maturin'), str(Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo'))) / 'bin/maturin')]
    maturin = next((c for c in candidates if c and Path(c).is_file() and _version(c) == 'maturin 1.14.1'), None)
    if not maturin:
        raise MaturinUnavailable('maturin 1.14.1 is required; run bash scripts/setup-packaging.sh')
    command = [maturin, 'pep517', 'write-dist-info', '--bindings', 'bin', '--target', target, '--locked', '--metadata-directory', str(into)]
    if target == 'x86_64-unknown-linux-gnu':
        command += ['--compatibility', 'linux']
    # Its progress goes to stderr: a caller's stdout is its own result.
    rerun = f'rerun `{shlex.join(command)}` from {root} to see why'
    try:
        status = subprocess.run(command, cwd=root, stdout=sys.stderr).returncode
    except OSError as error:
        raise MaturinUnavailable(f'maturin could not run ({error}); {rerun}') from None
    if status != 0:
        raise MaturinUnavailable(f'maturin could not describe the {target} wheel (exit {status}); {rerun}')
    infos = list(into.glob('*.dist-info'))
    if len(infos) != 1:
        raise MaturinUnavailable('maturin produced ambiguous metadata; inspect the metadata producer output')
    return infos[0]


def _version(maturin: str) -> str | None:
    """What `maturin --version` prints, or None when that maturin cannot run."""
    try:
        return subprocess.run([maturin, '--version'], capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None
