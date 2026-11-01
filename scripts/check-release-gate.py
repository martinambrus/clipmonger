#!/usr/bin/env python3
"""Exercise the real release script with isolated command spies, not Rust tests."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--fixture-dir', required=True, type=Path)
parser.add_argument('--script', type=Path, default=Path(__file__).with_name('release-gate.sh'))
args = parser.parse_args()
args.fixture_dir.mkdir(parents=True, exist_ok=False)
script = args.script.resolve().read_bytes()
spy = '''#!{python}
import json, os, sys
from pathlib import Path
name = Path(sys.argv[0]).name
argv = sys.argv[1:]
with open(os.environ['SPY_LOG'], 'a') as log:
    log.write(json.dumps([name, *argv]) + '\\n')
mode = os.environ['SPY_MODE']
if name == 'cargo' and argv[:1] == ['clippy'] and '--target' in argv and mode == 'windows-clippy-fails':
    sys.exit(23)
if name == 'cargo' and argv[:2] == ['deny', 'check'] and mode == 'deny-fails':
    sys.exit(24)
sys.exit(0)
'''.format(python=sys.executable)


def executable(path, data):
    with os.fdopen(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o755), 'w') as file:
        file.write(data)


failures = []
for mode in ['all-pass', 'windows-clippy-fails', 'deny-fails', 'audit-only', 'no-audit-tools']:
    root = args.fixture_dir / mode
    bin_dir = root / 'bin'
    bin_dir.mkdir(parents=True)
    (root / 'scripts').mkdir()
    (root / 'scripts/release-gate.sh').write_bytes(script)
    (root / 'gnome-ext').mkdir()
    (root / 'docs').mkdir()
    (root / 'CHANGELOG.md').write_text('fixture\n')
    (root / 'docs/release-notes-0.1.0.md').write_text('fixture\n')
    for command in ['bash', 'dirname', 'timeout']:
        (bin_dir / command).symlink_to(shutil.which(command))
    for command in ['cargo', 'npm']:
        executable(bin_dir / command, spy)
    if mode not in ['audit-only', 'no-audit-tools']:
        executable(bin_dir / 'cargo-deny', spy)
    if mode == 'audit-only':
        executable(bin_dir / 'cargo-audit', spy)
    log = root / 'argv.jsonl'
    env = os.environ | {'PATH': str(bin_dir.resolve()), 'SPY_LOG': str(log.resolve()), 'SPY_MODE': mode}
    result = subprocess.run([str(bin_dir.resolve() / 'bash'), str(root.resolve() / 'scripts/release-gate.sh')],
                            env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (root / 'stdout.log').write_text(result.stdout)
    calls = [json.loads(line) for line in log.read_text().splitlines()]
    expected_clippy = ['cargo', 'clippy', '--workspace', '--all-targets', '--all-features',
                       '--target', 'x86_64-pc-windows-gnu', '--', '-D', 'warnings']
    expected_check = ['cargo', 'check', '--workspace', '--all-targets', '--all-features',
                      '--target', 'x86_64-pc-windows-gnu']
    checks = {
        'windows-clippy-argv': calls.count(expected_clippy) == 1,
        'windows-check-argv': calls.count(expected_check) == 1,
        'host-clippy-argv': ['cargo', 'clippy', '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings'] in calls,
        'npm-build-and-test': ['npm', 'run', 'build'] in calls and ['npm', 'test'] in calls,
        'manual-boundaries': 'SKIP-MANUAL gate1-windows-session-suite:' in result.stdout and 'SKIP-MANUAL gate1-gnome-legs:' in result.stdout,
        'direct-status': result.returncode == (0 if mode == 'all-pass' else 1),
        'gate1-status': ('GATE 1 full-pyramid-matrix: ' + ('FAIL' if mode == 'windows-clippy-fails' else 'PASS')) in result.stdout,
        'gate6-status': ('GATE 6 dependency-review: ' + ('FAIL' if mode in ['deny-fails', 'audit-only', 'no-audit-tools'] else 'PASS')) in result.stdout,
    }
    if mode == 'windows-clippy-fails':
        checks['clippy-failure-propagated'] = 'FAIL windows-gnu-clippy (exit 23)' in result.stdout
    if mode == 'audit-only':
        checks['advisory-only-fallback'] = ['cargo', 'audit'] in calls and 'GATE 6 dependency-review: PASS' not in result.stdout
    (root / 'result.json').write_text(json.dumps({'exit': result.returncode, 'checks': checks}, indent=2) + '\n')
    failed = [key for key, passed in checks.items() if not passed]
    print(mode + ': ' + ('FAIL ' + ', '.join(failed) if failed else 'PASS'))
    failures.extend(mode + ': ' + key for key in failed)
print('SCRIPT-contract fixtures only; no real Cargo/npm commands or Rust suite executed.')
sys.exit(bool(failures))
