#!/usr/bin/env python3
"""Compare an NSIS installer's embedded payload with the Windows portable ZIP."""
import argparse
from pathlib import Path
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('installer', type=Path)
parser.add_argument('portable_zip', type=Path)
args = parser.parse_args()
def members(path):
    listing = subprocess.run(['7z', 'l', '-slt', str(path)], check=True, capture_output=True, text=True).stdout
    if 'Type = Nsis' not in listing or '\n----------\n' not in listing:
        raise ValueError('not an inspectable NSIS archive')
    return [line[7:] for line in listing.split('\n----------\n', 1)[1].splitlines() if line.startswith('Path = ')]


def check_members(entries, expected, label):
    if len(entries) != len(set(entries)) or set(entries) != expected:
        raise ValueError(f'unexpected {label} members: missing={sorted(expected - set(entries))}, extra={sorted(set(entries) - expected)}')


def extract(path, name):
    return subprocess.run(['7z', 'x', '-so', str(path), name], check=True, capture_output=True).stdout


try:
    entries = members(args.installer)
    with zipfile.ZipFile(args.portable_zip) as archive:
        payload = {name: archive.read(name) for name in archive.namelist() if name not in ('portable.marker', 'README.txt')}
    payload['cleanup.ps1'] = (ROOT / 'packaging/windows/cleanup.ps1').read_bytes()
    payload['profile-removal.cs'] = (ROOT / 'packaging/windows/profile-removal.cs').read_bytes()
    expected = set(payload) | {'uninstall.exe', '$PLUGINSDIR/modern-wizard.bmp', '$PLUGINSDIR/nsDialogs.dll', '$PLUGINSDIR/System.dll'}
    check_members(entries, expected, 'installer')
    for name, expected_bytes in payload.items():
        if extract(args.installer, name) != expected_bytes:
            raise ValueError(f'installer payload differs: {name}')
    with tempfile.TemporaryDirectory(prefix='clipmonger-uninstaller-') as temp:
        uninstaller = Path(temp) / 'uninstall.exe'
        uninstaller.write_bytes(extract(args.installer, 'uninstall.exe'))
        expected_uninstaller = {'cleanup.ps1', 'profile-removal.cs', '$PLUGINSDIR/modern-wizard.bmp', '$PLUGINSDIR/nsDialogs.dll', '$PLUGINSDIR/System.dll', '$PLUGINSDIR/nsExec.dll'}
        check_members(members(uninstaller), expected_uninstaller, 'uninstaller')
        for name in ('cleanup.ps1', 'profile-removal.cs'):
            if extract(uninstaller, name) != payload[name]:
                raise ValueError(f'uninstaller payload differs: {name}')
    print('Installer embedded payload: PASS; execution and Windows cleanup remain unverified.')
except (OSError, ValueError, zipfile.BadZipFile, subprocess.CalledProcessError) as exc:
    print(f'installer contents: {exc}', file=sys.stderr)
    sys.exit(1)
