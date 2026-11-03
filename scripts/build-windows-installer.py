#!/usr/bin/env python3
"""Compile a draft installer from prepared binaries and license payload."""
import argparse
import os
from pathlib import Path
import subprocess
import sys
from notice_provenance import verified_notices

ROOT = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--input', type=Path, required=True, help='prepared Windows binaries and licenses/')
parser.add_argument('--output', type=Path, required=True, help='new installer path')
parser.add_argument('--makensis', type=Path, required=True)
parser.add_argument('--nsisdir', type=Path, required=True)
parser.add_argument('--scan-data', required=True, type=Path)
parser.add_argument('--scan-receipt', required=True, type=Path)
parser.add_argument('--scan-tool', required=True, type=Path)
args = parser.parse_args()
try:
    source = args.input.resolve(strict=True)
    output = args.output.absolute()
    compiler = args.makensis.resolve(strict=True)
    nsisdir = args.nsisdir.resolve(strict=True)
    if output.exists() or output.is_symlink():
        raise ValueError('output already exists')
    if not output.parent.is_dir():
        raise ValueError('output parent directory does not exist')
    if any(ch in str(path) for path in (source, output) for ch in ('"', '$', '!', '\\', '\n', '\r')):
        raise ValueError('path contains unsupported NSIS metacharacters')
    required = [source / (name + '.exe') for name in ('clipmonger', 'clipmonger-daemon', 'clipmonger-cli')]
    required.append(source / 'licenses/RUST-NOTICES.txt')
    for path in required:
        if path.is_symlink() or not path.is_file() or not path.stat().st_size:
            raise ValueError(f'missing, empty or symlinked input: {path}')
    if (source / 'licenses/RUST-NOTICES.txt').read_bytes() != verified_notices(args.scan_data, args.scan_receipt, args.scan_tool):
        raise ValueError('notices differ from verified scan')
    for path in (ROOT / 'licenses').rglob('*'):
        if path.is_symlink():
            raise ValueError(f'symlink in license source: {path}')
        if path.is_file():
            other = source / 'licenses' / path.relative_to(ROOT / 'licenses')
            if other.is_symlink() or not other.is_file() or other.read_bytes() != path.read_bytes():
                raise ValueError(f'license payload differs from source: {other}')
    expected = {p.relative_to(ROOT / 'licenses').as_posix() for p in (ROOT / 'licenses').rglob('*') if p.is_file()}
    expected.add('RUST-NOTICES.txt')
    actual = set()
    for path in (source / 'licenses').rglob('*'):
        if path.is_symlink():
            raise ValueError(f'symlink in license payload: {path}')
        if path.is_file():
            actual.add(path.relative_to(source / 'licenses').as_posix())
    if actual != expected:
        raise ValueError(f'license payload members differ: missing={sorted(expected - actual)}, unexpected={sorted(actual - expected)}')
    # No caller-controlled shell: NSIS receives separate arguments and a local tool directory.
    env = os.environ | {'NSISDIR': str(nsisdir)}
    result = subprocess.run([str(compiler), '-WX', f'-DINPUT_DIR={source}', f'-DOUTPUT_FILE={output}',
                             str(ROOT / 'packaging/windows/installer.nsi')], env=env)
    if result.returncode or not output.is_file():
        raise ValueError(f'makensis failed: {result.returncode}')
    print(f'Compiled draft installer: {output}')
    print('No installer execution or native cleanup validation was performed.')
except (OSError, ValueError) as exc:
    print(f'build-windows-installer: {exc}', file=sys.stderr)
    sys.exit(1)
