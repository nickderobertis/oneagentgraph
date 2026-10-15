"""What each release target ships, read from an executable's own header.

Shared by scripts/release-package.py, which refuses to wrap a binary built for
another target, and scripts/release-verify.py, which checks every staged payload
before publication. Reading the header is what lets a Linux host hold a Darwin
artifact to its contract without executing it.
"""
import struct

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
