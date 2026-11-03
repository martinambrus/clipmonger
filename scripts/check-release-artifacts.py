#!/usr/bin/env python3
"""Check portable ZIPs and checksums in a prepared release directory."""
import argparse
import hashlib
from pathlib import Path
import re
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--version', required=True)
parser.add_argument('directory', type=Path)
parser.add_argument('--scan-data', required=True, type=Path)
parser.add_argument('--scan-receipt', required=True, type=Path)
parser.add_argument('--scan-tool', required=True, type=Path)
args = parser.parse_args()
if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.]+)?', args.version):
    parser.error('invalid version')
files = [f'clipmonger-{args.version}-{platform}-x86_64.zip' for platform in ('windows', 'linux')]
files.append(f'clipmonger-{args.version}-windows-x86_64-setup.exe')
try:
    expected = []
    for name in files:
        path = args.directory / name
        if path.is_symlink() or not path.is_file() or not path.stat().st_size:
            raise ValueError(f'missing, empty or symlinked artifact: {name}')
        with path.open('rb') as f:
            digest = hashlib.file_digest(f, 'sha256').hexdigest()
        expected.append(f'{digest}  {name}\n')
    manifest = args.directory / 'SHA256SUMS'
    if manifest.is_symlink() or manifest.read_text(encoding='ascii') != ''.join(sorted(expected, key=lambda line: line.split('  ', 1)[1])):
        raise ValueError('SHA256SUMS does not exactly match release artifacts')
    for platform, name in zip(('windows', 'linux'), files):
        subprocess.run([sys.executable, str(Path(__file__).with_name('package-portable.py')),
                        'check', '--platform', platform, str(args.directory / name), '--scan-data', str(args.scan_data), '--scan-receipt', str(args.scan_receipt), '--scan-tool', str(args.scan_tool)], check=True)
    subprocess.run([sys.executable, str(Path(__file__).with_name('check-windows-installer.py')),
                    str(args.directory / files[2]), str(args.directory / files[0])], check=True)
    print('Portable and installer payloads and all release checksums: PASS')
    print('Native installer execution requires separate verification.')
except (OSError, ValueError, UnicodeError, subprocess.CalledProcessError) as error:
    print(f'release artifact check: {error}', file=sys.stderr)
    sys.exit(1)
