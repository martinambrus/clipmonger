#!/usr/bin/env python3
"""Run the guarded notice scan and write its receipt (scripts/NOTICE-SCAN.md).

Usage: notice-scan-guard.py --receipt RECEIPT.json -- cargo SUBCOMMAND --tool T --config C --output-file O

Before and after the command it hashes the Rust and Cargo inputs of the
checkout (every Cargo.toml, Cargo.lock, deny.toml, rust-toolchain.toml and
every .rs file under crates/). The receipt passes only when the command exits
0, the inputs are unchanged, the output file exists and is non-empty, and the
command finished within the time limit with no child left running.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
TIME_LIMIT_S = 1800


def inputs():
    names = ['Cargo.toml', 'Cargo.lock', 'deny.toml', 'rust-toolchain.toml']
    names += sorted(str(p.relative_to(ROOT)) for p in (ROOT / 'crates').rglob('Cargo.toml'))
    names += sorted(str(p.relative_to(ROOT)) for p in (ROOT / 'crates').rglob('*.rs'))
    return {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
            for name in names if (ROOT / name).is_file()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--receipt', required=True, type=Path)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command:
        parser.error('no command')
    output = Path(command[-1])
    before = inputs()
    resource_violation = False
    # Own process group, so a timeout also stops anything the command spawned.
    process = subprocess.Popen(command, cwd=ROOT, start_new_session=True)
    try:
        exit_code = process.wait(timeout=TIME_LIMIT_S)
    except subprocess.TimeoutExpired:
        resource_violation = True
        os.killpg(process.pid, signal.SIGKILL)
        exit_code = process.wait()
    try:
        os.killpg(process.pid, 0)
        cleanup = False
    except ProcessLookupError:
        cleanup = True
    after = inputs()
    changed = before != after
    complete = output.is_file() and output.stat().st_size > 0
    passed = exit_code == 0 and not changed and cleanup and complete and not resource_violation
    receipt = {
        'status': 'PASS' if passed else 'FAIL',
        'direct_exit': exit_code,
        'effective_gate_exit': 0 if passed else 1,
        'source_changed': changed,
        'cleanup_verified': cleanup,
        'evidence_complete': complete,
        'resource_violation': resource_violation,
        'next_calls_must_hold': not passed,
        'command': command,
        'before': before,
        'after': after,
    }
    with args.receipt.open('x') as f:
        json.dump(receipt, f, indent=2)
        f.write('\n')
    print(f"guard: {receipt['status']} (exit {exit_code}, inputs {'changed' if changed else 'unchanged'})")
    return 0 if passed else 1


if __name__ == '__main__':
    sys.exit(main())
